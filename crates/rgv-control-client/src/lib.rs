//! Shared authenticated client for one explicitly enabled RGV desktop control service.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use reqwest::StatusCode;
use rgv_automation::{
    AutomationBatch, AutomationCommand, AutomationRunReport, StressHealthSnapshot,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::sync::Mutex;
use url::Url;

const MAXIMUM_CONTROL_RESPONSE_BYTES: usize = 80 * 1024 * 1024;

#[derive(Clone)]
pub struct ControlClient {
    client: reqwest::Client,
    base_url: Url,
    secret: Arc<str>,
    gate: Arc<Mutex<()>>,
    uncertain_mutation: Arc<AtomicBool>,
    observed_instance_id: Arc<Mutex<Option<String>>>,
}

impl ControlClient {
    pub fn new(base_url: Url, secret: String) -> Result<Self, ControlClientError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(610))
            .build()?;
        Ok(Self {
            client,
            base_url,
            secret: Arc::from(secret),
            gate: Arc::new(Mutex::new(())),
            uncertain_mutation: Arc::new(AtomicBool::new(false)),
            observed_instance_id: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn status(&self) -> Result<ControlStatus, ControlClientError> {
        let parsed: ControlStatus = self.get_json("v1/status", false).await?;
        let mut observed = self.observed_instance_id.lock().await;
        if observed
            .as_ref()
            .is_some_and(|previous| previous != &parsed.application_instance_id)
        {
            self.uncertain_mutation.store(false, Ordering::Release);
        }
        *observed = Some(parsed.application_instance_id.clone());
        Ok(parsed)
    }

    pub async fn health(&self) -> Result<StressHealthSnapshot, ControlClientError> {
        self.get_json("v1/stress/health", true).await
    }

    pub async fn run_batch(
        &self,
        batch: &AutomationBatch,
    ) -> Result<AutomationRunReport, ControlClientError> {
        self.run_batch_inner(batch, None).await
    }

    /// Runs a short control batch with a caller-owned outer deadline.
    ///
    /// Stress controllers use this for observation scopes so a blocked UI cannot hold the
    /// external watchdog for the client's normal ten-minute automation allowance.
    pub async fn run_batch_with_timeout(
        &self,
        batch: &AutomationBatch,
        timeout: Duration,
    ) -> Result<AutomationRunReport, ControlClientError> {
        self.run_batch_inner(batch, Some(timeout)).await
    }

    async fn run_batch_inner(
        &self,
        batch: &AutomationBatch,
        timeout: Option<Duration>,
    ) -> Result<AutomationRunReport, ControlClientError> {
        batch.validate()?;
        let _guard = self.gate.lock().await;
        let mutates_viewer = batch_mutates_viewer(batch);
        if mutates_viewer && self.uncertain_mutation.load(Ordering::Acquire) {
            // Stamp the observation with the caller's (already validated) schema version so a
            // caller targeting an older supported viewer is not rejected by this internal probe.
            let observation = AutomationBatch {
                schema_version: batch.schema_version,
                commands: vec![AutomationCommand::DescribeWorkspace],
            };
            let report = self.send_batch_locked(&observation, timeout).await?;
            if !report.succeeded {
                return Err(ControlClientError::StateReobservationFailed);
            }
            self.uncertain_mutation.store(false, Ordering::Release);
        }
        let mut uncertainty =
            mutates_viewer.then(|| MutationUncertainty::new(Arc::clone(&self.uncertain_mutation)));
        let result = self.send_batch_locked(batch, timeout).await;
        if result.is_ok()
            && let Some(uncertainty) = uncertainty.as_mut()
        {
            uncertainty.complete();
        }
        result
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        authenticated: bool,
    ) -> Result<T, ControlClientError> {
        let mut request = self.client.get(self.endpoint(path));
        request = request.timeout(Duration::from_secs(3));
        if authenticated {
            request = request.header("Authorization", format!("Bearer {}", self.secret));
        }
        let response = request.send().await?;
        decode_response(response).await
    }

    async fn send_batch_locked(
        &self,
        batch: &AutomationBatch,
        timeout: Option<Duration>,
    ) -> Result<AutomationRunReport, ControlClientError> {
        let body = serde_json::to_vec(batch)?;
        let mut request = self
            .client
            .post(self.endpoint("v1/batches"))
            .header("Authorization", format!("Bearer {}", self.secret))
            .header("Content-Type", "application/json")
            .body(body);
        if let Some(timeout) = timeout {
            request = request.timeout(timeout);
        }
        let response = request.send().await?;
        decode_response(response).await
    }

    fn endpoint(&self, path: &str) -> Url {
        let mut endpoint = self.base_url.clone();
        endpoint.set_path(path);
        endpoint
    }
}

fn batch_mutates_viewer(batch: &AutomationBatch) -> bool {
    batch.commands.iter().any(command_mutates_viewer)
}

/// Whether `command` can change the viewer's state, so that losing its response leaves the
/// client unsure what the viewer now shows.
///
/// The match is exhaustive on purpose (no wildcard arm): a command added to `rgv-automation`
/// does not compile here until it is classified, so it cannot default to either class.
/// Commands that only write a file the caller named (`snapshot`, `export`, `save`) or only wait
/// do not change viewer state and count as read-only.
const fn command_mutates_viewer(command: &AutomationCommand) -> bool {
    match command {
        // Pure observations of the workspace, a selection, a queue, a history or loaded tracks.
        AutomationCommand::DescribeWorkspace
        | AutomationCommand::DescribeSelection
        | AutomationCommand::DescribeInteractionTrace
        | AutomationCommand::DescribeInteractionRecording
        | AutomationCommand::DescribeReviewQueue
        | AutomationCommand::DescribeNavigationHistory { .. }
        | AutomationCommand::DescribeRelatedAlignments
        | AutomationCommand::QueryFeatures { .. }
        | AutomationCommand::WaitUntilSettled { .. }
        | AutomationCommand::Snapshot { .. }
        | AutomationCommand::Export { .. }
        | AutomationCommand::Save { .. } => false,
        // Recording and tracing state.
        AutomationCommand::StartInteractionRecording { .. }
        | AutomationCommand::StopInteractionRecording
        | AutomationCommand::BeginNativeObservation { .. }
        | AutomationCommand::EndNativeObservation
        | AutomationCommand::CheckpointInteractionRecording { .. }
        | AutomationCommand::StartDiagnosticTrace
        | AutomationCommand::StopDiagnosticTrace
        // Selection, workspace content, viewport and display state. `find_alignments`
        // invalidates the handles of earlier calls, and `goto_gene` navigates a panel.
        | AutomationCommand::ClearSelection
        | AutomationCommand::RefreshSelection
        | AutomationCommand::NewWorkspace
        | AutomationCommand::OpenWorkspace { .. }
        | AutomationCommand::SetGenome { .. }
        | AutomationCommand::LoadTrack { .. }
        | AutomationCommand::RemoveTrack { .. }
        | AutomationCommand::Goto { .. }
        | AutomationCommand::ChangeViewport { .. }
        | AutomationCommand::AddPanel { .. }
        | AutomationCommand::ClosePanel { .. }
        | AutomationCommand::SetOption { .. }
        | AutomationCommand::Configure { .. }
        | AutomationCommand::Group { .. }
        | AutomationCommand::Sort { .. }
        | AutomationCommand::Filter { .. }
        | AutomationCommand::Select { .. }
        | AutomationCommand::FindAlignments { .. }
        | AutomationCommand::SelectAlignment { .. }
        | AutomationCommand::NavigateRelatedAlignment { .. }
        | AutomationCommand::NavigateReviewItem { .. }
        | AutomationCommand::NavigateHistory { .. }
        | AutomationCommand::GotoGene { .. }
        | AutomationCommand::Exit => true,
    }
}

struct MutationUncertainty {
    flag: Arc<AtomicBool>,
    completed: bool,
}

impl MutationUncertainty {
    fn new(flag: Arc<AtomicBool>) -> Self {
        Self {
            flag,
            completed: false,
        }
    }

    fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for MutationUncertainty {
    fn drop(&mut self) {
        if !self.completed {
            self.flag.store(true, Ordering::Release);
        }
    }
}

async fn decode_response<T: DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, ControlClientError> {
    let status = response.status();
    require_json_content_type(&response)?;
    let body = bounded_body(response).await?;
    if !status.is_success() {
        return Err(control_http_error(status, &body));
    }
    serde_json::from_slice(&body).map_err(ControlClientError::InvalidResponse)
}

fn require_json_content_type(response: &reqwest::Response) -> Result<(), ControlClientError> {
    let Some(content_type) = response.headers().get(reqwest::header::CONTENT_TYPE) else {
        return Err(ControlClientError::UnexpectedContentType);
    };
    let Ok(content_type) = content_type.to_str() else {
        return Err(ControlClientError::UnexpectedContentType);
    };
    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    if media_type.eq_ignore_ascii_case("application/json")
        || media_type.to_ascii_lowercase().ends_with("+json")
    {
        Ok(())
    } else {
        Err(ControlClientError::UnexpectedContentType)
    }
}

async fn bounded_body(mut response: reqwest::Response) -> Result<Vec<u8>, ControlClientError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAXIMUM_CONTROL_RESPONSE_BYTES as u64)
    {
        return Err(ControlClientError::ResponseTooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len().saturating_add(chunk.len()) > MAXIMUM_CONTROL_RESPONSE_BYTES {
            return Err(ControlClientError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn control_http_error(status: StatusCode, body: &[u8]) -> ControlClientError {
    let parsed = serde_json::from_slice::<ControlErrorEnvelope>(body).ok();
    ControlClientError::Http {
        status: status.as_u16(),
        code: parsed
            .as_ref()
            .map_or_else(|| "http_error".to_owned(), |value| value.error.code.clone()),
        message: parsed.map_or_else(
            || "RGV local control returned an unsuccessful response".to_owned(),
            |value| value.error.message,
        ),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ControlStatus {
    pub schema_version: u32,
    #[serde(default)]
    pub control_schema_version: Option<u32>,
    #[serde(default)]
    pub application_version: String,
    #[serde(default)]
    pub application_instance_id: String,
    #[serde(default)]
    pub process_id: u32,
    #[serde(default)]
    pub window_count: u32,
    #[serde(default)]
    pub active_window_ordinal: u32,
    #[serde(default)]
    pub renderer_backend: String,
    #[serde(default)]
    pub surface_width_pixels: u32,
    #[serde(default)]
    pub surface_height_pixels: u32,
    #[serde(default)]
    pub scale_factor: f64,
    pub service: String,
    pub authentication: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ControlErrorEnvelope {
    error: ControlErrorBody,
}

#[derive(Debug, Deserialize)]
struct ControlErrorBody {
    code: String,
    message: String,
}

#[derive(Debug, Error)]
pub enum ControlClientError {
    #[error("could not communicate with the RGV window: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("could not encode or decode RGV control JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the RGV automation batch is invalid: {0}")]
    InvalidBatch(#[from] rgv_automation::AutomationError),
    #[error("RGV local control returned {status} ({code}): {message}")]
    Http {
        status: u16,
        code: String,
        message: String,
    },
    #[error("RGV local control returned malformed JSON: {0}")]
    InvalidResponse(serde_json::Error),
    #[error("RGV local control response exceeded the bounded response limit")]
    ResponseTooLarge,
    #[error("RGV local control returned an unexpected content type")]
    UnexpectedContentType,
    #[error("RGV state could not be re-observed after an interrupted mutation")]
    StateReobservationFailed,
}

#[cfg(test)]
mod tests {
    use std::thread;

    use rgv_automation::AUTOMATION_SCHEMA_VERSION;
    use tiny_http::{Header, Response, Server};

    use super::*;

    fn header(name: &str, value: &str) -> Header {
        Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap()
    }

    #[tokio::test]
    async fn health_uses_bearer_authentication() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let worker = thread::spawn(move || {
            let request = server.recv().unwrap();
            let authorization = request
                .headers()
                .iter()
                .find(|candidate| candidate.field.equiv("Authorization"))
                .map(|candidate| candidate.value.as_str().to_owned());
            request
                .respond(
                    Response::from_string(r#"{"schema_version":1,"application_instance_id":"a","process_id":4,"sampled_unix_millis":1,"ui_heartbeat_sequence":2,"ui_last_elapsed_millis":3,"native_input_sequence":4,"frame_sequence":5,"desired_generation":6,"committed_generation":6,"worker_progress_sequence":7,"worker_last_elapsed_millis":8,"interaction_state":"idle","recording":{"state":"inactive"}}"#)
                        .with_header(header("Content-Type", "application/json")),
                )
                .unwrap();
            authorization
        });
        let client = ControlClient::new(
            Url::parse(&format!("http://{address}")).unwrap(),
            "protected-secret-0123456789abcdef".to_owned(),
        )
        .unwrap();
        assert_eq!(client.health().await.unwrap().process_id, 4);
        assert_eq!(
            worker.join().unwrap().as_deref(),
            Some("Bearer protected-secret-0123456789abcdef")
        );
    }

    #[tokio::test]
    async fn batches_retain_existing_wire_format() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let worker = thread::spawn(move || {
            let mut request = server.recv().unwrap();
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            request
                .respond(
                    Response::from_string(r#"{"schema_version":6,"succeeded":true,"results":[]}"#)
                        .with_header(header("Content-Type", "application/json")),
                )
                .unwrap();
            body
        });
        let client = ControlClient::new(
            Url::parse(&format!("http://{address}")).unwrap(),
            "protected-secret-0123456789abcdef".to_owned(),
        )
        .unwrap();
        client
            .run_batch(&AutomationBatch {
                schema_version: AUTOMATION_SCHEMA_VERSION,
                commands: vec![AutomationCommand::DescribeWorkspace],
            })
            .await
            .unwrap();
        assert!(worker.join().unwrap().contains("describe_workspace"));
    }
    fn command(json: serde_json::Value) -> AutomationCommand {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn read_only_commands_are_not_treated_as_mutations() {
        for read_only in [
            command(serde_json::json!({"command": "describe_workspace"})),
            command(serde_json::json!({"command": "describe_review_queue"})),
            command(serde_json::json!({"command": "describe_related_alignments"})),
            command(serde_json::json!({"command": "describe_navigation_history", "panel_id": 1})),
            command(serde_json::json!({
                "command": "query_features", "contig": "chr1", "start": 0, "end": 1000
            })),
            command(serde_json::json!({"command": "wait_until_settled"})),
        ] {
            assert!(
                !command_mutates_viewer(&read_only),
                "{} is read-only",
                read_only.name()
            );
        }
        for mutating in [
            command(serde_json::json!({"command": "goto_gene", "query": "TP53"})),
            command(serde_json::json!({"command": "clear_selection"})),
            command(serde_json::json!({"command": "new_workspace"})),
            command(serde_json::json!({"command": "exit"})),
        ] {
            assert!(
                command_mutates_viewer(&mutating),
                "{} changes the viewer",
                mutating.name()
            );
        }
        let batch = |commands| AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands,
        };
        assert!(!batch_mutates_viewer(&batch(vec![
            AutomationCommand::DescribeWorkspace,
            command(serde_json::json!({
                "command": "query_features", "contig": "chr1", "start": 0, "end": 1000
            })),
        ])));
        assert!(batch_mutates_viewer(&batch(vec![
            AutomationCommand::DescribeWorkspace,
            command(serde_json::json!({"command": "goto_gene", "query": "TP53"})),
        ])));
    }

    /// The variants of `AutomationCommand`, read from the `name()` match in its source (which
    /// the compiler keeps exhaustive).
    fn automation_command_variants() -> Vec<String> {
        let source = include_str!("../../rgv-automation/src/automation.rs");
        let start = source
            .find("pub const fn name(&self) -> &'static str {")
            .expect("AutomationCommand::name exists");
        let body = &source[start..];
        let body = &body[..body.find("\n    }\n").expect("name() ends")];
        body.lines()
            .filter_map(|line| line.trim().strip_prefix("Self::"))
            .map(|rest| {
                rest.chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn every_automation_command_has_an_explicit_classification() {
        let variants = automation_command_variants();
        assert!(variants.len() >= 43, "found only {}", variants.len());
        let source = include_str!("lib.rs");
        let start = source
            .find("const fn command_mutates_viewer")
            .expect("the classifier exists");
        let classifier = &source[start..];
        let classifier = &classifier[..classifier.find("\n}\n").expect("classifier ends")];
        // No arm may catch commands it does not name.
        for line in classifier.lines() {
            let line = line.trim();
            assert!(
                !(line.starts_with("_ ") || line.starts_with("| _ ") || line.starts_with("_ =>")),
                "the classifier must not have a wildcard arm: {line}"
            );
        }
        for variant in &variants {
            let named = format!("AutomationCommand::{variant}");
            let occurrences = classifier
                .match_indices(&named)
                .filter(|(index, _)| {
                    !classifier[index + named.len()..]
                        .starts_with(|next: char| next.is_ascii_alphanumeric())
                })
                .count();
            assert_eq!(occurrences, 1, "{variant} must be classified exactly once");
        }
    }
}
