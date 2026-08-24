use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use reqwest::{StatusCode, Url};
use rgv_core::{
    AUTOMATION_SCHEMA_VERSION, AutomationBatch, AutomationCommand, AutomationRunReport,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;

const MAXIMUM_CONTROL_RESPONSE_BYTES: usize = 80 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct ControlClient {
    client: reqwest::Client,
    base_url: Url,
    secret: Arc<str>,
    gate: Arc<Mutex<()>>,
    uncertain_mutation: Arc<AtomicBool>,
    observed_instance_id: Arc<Mutex<Option<String>>>,
}

impl ControlClient {
    pub(crate) fn new(base_url: Url, secret: String) -> Result<Self, ControlClientError> {
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

    pub(crate) async fn status(&self) -> Result<ControlStatus, ControlClientError> {
        let response = self.client.get(self.endpoint("v1/status")).send().await?;
        let status = response.status();
        require_json_content_type(&response)?;
        let body = bounded_body(response).await?;
        if !status.is_success() {
            return Err(control_http_error(status, &body));
        }
        let parsed: ControlStatus =
            serde_json::from_slice(&body).map_err(ControlClientError::InvalidResponse)?;
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

    pub(crate) async fn run_batch(
        &self,
        batch: &AutomationBatch,
    ) -> Result<AutomationRunReport, ControlClientError> {
        batch.validate()?;
        let _guard = self.gate.lock().await;
        let mutates_viewer = batch_mutates_viewer(batch);
        if mutates_viewer && self.uncertain_mutation.load(Ordering::Acquire) {
            let observation = AutomationBatch {
                schema_version: AUTOMATION_SCHEMA_VERSION,
                commands: vec![AutomationCommand::DescribeWorkspace],
            };
            let report = self.send_batch_locked(&observation).await?;
            if !report.succeeded {
                return Err(ControlClientError::StateReobservationFailed);
            }
            self.uncertain_mutation.store(false, Ordering::Release);
        }
        let mut uncertainty =
            mutates_viewer.then(|| MutationUncertainty::new(Arc::clone(&self.uncertain_mutation)));
        let result = self.send_batch_locked(batch).await;
        if result.is_ok()
            && let Some(uncertainty) = uncertainty.as_mut()
        {
            uncertainty.complete();
        }
        result
    }

    async fn send_batch_locked(
        &self,
        batch: &AutomationBatch,
    ) -> Result<AutomationRunReport, ControlClientError> {
        let body = serde_json::to_vec(batch)?;
        let response = self
            .client
            .post(self.endpoint("v1/batches"))
            .header("Authorization", format!("Bearer {}", self.secret))
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await?;
        let status = response.status();
        require_json_content_type(&response)?;
        let body = bounded_body(response).await?;
        if !status.is_success() {
            return Err(control_http_error(status, &body));
        }
        serde_json::from_slice(&body).map_err(ControlClientError::InvalidResponse)
    }

    fn endpoint(&self, path: &str) -> Url {
        let mut endpoint = self.base_url.clone();
        endpoint.set_path(path);
        endpoint
    }
}

fn batch_mutates_viewer(batch: &AutomationBatch) -> bool {
    batch.commands.iter().any(|command| {
        !matches!(
            command,
            AutomationCommand::DescribeWorkspace
                | AutomationCommand::DescribeSelection
                | AutomationCommand::WaitUntilSettled { .. }
                | AutomationCommand::Snapshot { .. }
                | AutomationCommand::Export { .. }
                | AutomationCommand::Save { .. }
        )
    })
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
pub(crate) struct ControlStatus {
    pub(crate) schema_version: u32,
    #[serde(default)]
    pub(crate) control_schema_version: Option<u32>,
    #[serde(default)]
    pub(crate) application_version: String,
    #[serde(default)]
    pub(crate) application_instance_id: String,
    #[serde(default)]
    pub(crate) process_id: u32,
    #[serde(default)]
    pub(crate) window_count: u32,
    #[serde(default)]
    pub(crate) active_window_ordinal: u32,
    #[serde(default)]
    pub(crate) renderer_backend: String,
    #[serde(default)]
    pub(crate) surface_width_pixels: u32,
    #[serde(default)]
    pub(crate) surface_height_pixels: u32,
    #[serde(default)]
    pub(crate) scale_factor: f64,
    pub(crate) service: String,
    pub(crate) authentication: String,
    #[serde(default)]
    pub(crate) capabilities: Vec<String>,
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
pub(crate) enum ControlClientError {
    #[error("could not communicate with the RGV window: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("could not encode or decode RGV control JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the RGV automation batch is invalid: {0}")]
    InvalidBatch(#[from] rgv_core::AutomationError),
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
    use std::{
        io::{Read as _, Write as _},
        net::TcpListener,
        thread,
    };

    use rgv_core::{AUTOMATION_SCHEMA_VERSION, AutomationCommand};
    use tiny_http::{Header, Response, Server, StatusCode};

    use super::*;

    struct ObservedRequest {
        authorization: Option<String>,
        body: String,
    }

    fn response_header(name: &str, value: &str) -> Header {
        Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap()
    }

    fn serve_once(
        status: u16,
        content_type: &str,
        body: &'static str,
    ) -> (Url, thread::JoinHandle<ObservedRequest>) {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let url = Url::parse(&format!("http://{address}")).unwrap();
        let content_type = content_type.to_owned();
        let worker = thread::spawn(move || {
            let mut request = server.recv().unwrap();
            let authorization = request
                .headers()
                .iter()
                .find(|header| header.field.equiv("Authorization"))
                .map(|header| header.value.as_str().to_owned());
            let mut request_body = String::new();
            request
                .as_reader()
                .read_to_string(&mut request_body)
                .unwrap();
            request
                .respond(
                    Response::from_string(body)
                        .with_status_code(StatusCode(status))
                        .with_header(response_header("Content-Type", &content_type)),
                )
                .unwrap();
            ObservedRequest {
                authorization,
                body: request_body,
            }
        });
        (url, worker)
    }

    #[tokio::test]
    async fn status_requires_json_and_parses_capabilities() {
        let (url, worker) = serve_once(
            200,
            "application/json; charset=utf-8",
            r#"{"schema_version":1,"control_schema_version":1,"application_version":"0.1.0","service":"rgv-local-control","authentication":"bearer","capabilities":["workspace_state"]}"#,
        );
        let client = ControlClient::new(url, "x".repeat(32)).unwrap();
        let status = client.status().await.unwrap();
        assert_eq!(status.service, "rgv-local-control");
        assert_eq!(status.capabilities, ["workspace_state"]);
        worker.join().unwrap();

        let (url, worker) = serve_once(200, "text/plain", "{}");
        let client = ControlClient::new(url, "x".repeat(32)).unwrap();
        assert!(matches!(
            client.status().await,
            Err(ControlClientError::UnexpectedContentType)
        ));
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn batch_uses_bearer_auth_and_preserves_failed_reports() {
        let (url, worker) = serve_once(
            200,
            "application/json",
            r#"{"schema_version":1,"succeeded":false,"results":[{"command_index":0,"command":"describe_workspace","status":"failed","error_code":"invalid_state","message":"not ready","data":{}}]}"#,
        );
        let client =
            ControlClient::new(url, "protected-secret-0123456789abcdef".to_owned()).unwrap();
        let batch = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::DescribeWorkspace],
        };
        let report = client.run_batch(&batch).await.unwrap();
        assert!(!report.succeeded);
        let observed = worker.join().unwrap();
        assert_eq!(
            observed.authorization.as_deref(),
            Some("Bearer protected-secret-0123456789abcdef")
        );
        assert!(observed.body.contains("describe_workspace"));
        assert!(!observed.body.contains("protected-secret"));
    }

    #[tokio::test]
    async fn concurrent_batches_do_not_overlap_on_the_control_transport() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let worker = thread::spawn(move || {
            let mut first = server.recv().unwrap();
            let mut first_body = String::new();
            first.as_reader().read_to_string(&mut first_body).unwrap();

            // The client deliberately makes no FIFO promise for independently
            // scheduled calls, but it must never admit a second batch while the
            // first batch is still awaiting its response.
            let early_second = server.recv_timeout(Duration::from_millis(150)).unwrap();
            let overlapped = early_second.is_some();
            first
                .respond(
                    Response::from_string(r#"{"schema_version":1,"succeeded":true,"results":[]}"#)
                        .with_header(response_header("Content-Type", "application/json")),
                )
                .unwrap();

            let mut second = early_second.unwrap_or_else(|| server.recv().unwrap());
            let mut second_body = String::new();
            second.as_reader().read_to_string(&mut second_body).unwrap();
            second
                .respond(
                    Response::from_string(r#"{"schema_version":1,"succeeded":true,"results":[]}"#)
                        .with_header(response_header("Content-Type", "application/json")),
                )
                .unwrap();
            (overlapped, [first_body, second_body])
        });

        let client = ControlClient::new(
            Url::parse(&format!("http://{address}")).unwrap(),
            "x".repeat(32),
        )
        .unwrap();
        let first = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::Goto {
                loci: vec!["chr1:1-100".to_owned()],
            }],
        };
        let second = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::Goto {
                loci: vec!["chr2:1-100".to_owned()],
            }],
        };

        let (first_result, second_result) =
            tokio::join!(client.run_batch(&first), client.run_batch(&second));
        first_result.unwrap();
        second_result.unwrap();
        let (overlapped, bodies) = worker.join().unwrap();
        assert!(!overlapped);
        assert!(bodies.iter().any(|body| body.contains("chr1:1-100")));
        assert!(bodies.iter().any(|body| body.contains("chr2:1-100")));
    }

    #[tokio::test]
    async fn authentication_and_malformed_json_have_distinct_errors() {
        let (url, worker) = serve_once(
            401,
            "application/json",
            r#"{"error":{"code":"unauthorized","message":"authentication failed"}}"#,
        );
        let client = ControlClient::new(url, "wrong-secret-0123456789abcdefgh".to_owned()).unwrap();
        let batch = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::DescribeWorkspace],
        };
        assert!(matches!(
            client.run_batch(&batch).await,
            Err(ControlClientError::Http { ref code, .. }) if code == "unauthorized"
        ));
        worker.join().unwrap();

        let (url, worker) = serve_once(200, "application/json", "not-json");
        let client = ControlClient::new(url, "x".repeat(32)).unwrap();
        assert!(matches!(
            client.status().await,
            Err(ControlClientError::InvalidResponse(_))
        ));
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn oversized_response_headers_fail_before_body_allocation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1_024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 83886081\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let client = ControlClient::new(
            Url::parse(&format!("http://{address}")).unwrap(),
            "x".repeat(32),
        )
        .unwrap();
        assert!(matches!(
            client.status().await,
            Err(ControlClientError::ResponseTooLarge)
        ));
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn an_unavailable_client_reconnects_on_a_later_call() {
        let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let url = Url::parse(&format!("http://{address}")).unwrap();
        let client = ControlClient::new(url, "x".repeat(32)).unwrap();
        assert!(matches!(
            client.status().await,
            Err(ControlClientError::Transport(_))
        ));

        let server = Server::http(address).unwrap();
        let worker = thread::spawn(move || {
            let request = server.recv().unwrap();
            request
                .respond(
                    Response::from_string(
                        r#"{"schema_version":1,"application_version":"0.1.0","service":"rgv-local-control","authentication":"bearer","capabilities":[]}"#,
                    )
                    .with_header(response_header("Content-Type", "application/json")),
                )
                .unwrap();
        });
        assert_eq!(client.status().await.unwrap().service, "rgv-local-control");
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn uncertain_mutation_state_is_observed_before_the_next_mutation() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let worker = thread::spawn(move || {
            let mut bodies = Vec::new();
            for _ in 0..2 {
                let mut request = server.recv().unwrap();
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body).unwrap();
                bodies.push(body);
                request
                    .respond(
                        Response::from_string(
                            r#"{"schema_version":1,"succeeded":true,"results":[]}"#,
                        )
                        .with_header(response_header("Content-Type", "application/json")),
                    )
                    .unwrap();
            }
            bodies
        });
        let client = ControlClient::new(
            Url::parse(&format!("http://{address}")).unwrap(),
            "x".repeat(32),
        )
        .unwrap();
        client.uncertain_mutation.store(true, Ordering::Release);
        let mutation = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::Goto {
                loci: vec!["chr1:1-100".to_owned()],
            }],
        };
        client.run_batch(&mutation).await.unwrap();
        let bodies = worker.join().unwrap();
        assert!(bodies[0].contains("describe_workspace"));
        assert!(bodies[1].contains("goto"));
        assert!(!client.uncertain_mutation.load(Ordering::Acquire));
    }

    #[test]
    fn dropping_an_incomplete_mutation_marks_state_uncertain() {
        let flag = Arc::new(AtomicBool::new(false));
        {
            let _guard = MutationUncertainty::new(Arc::clone(&flag));
        }
        assert!(flag.load(Ordering::Acquire));
    }
}
