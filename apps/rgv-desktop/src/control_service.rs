use std::{
    io::Read as _,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use rgv_core::{AUTOMATION_SCHEMA_VERSION, AutomationBatch, AutomationError, AutomationRunReport};
use tiny_http::{Header, Method, Request, Response, StatusCode};

use crate::stress_health::StressHealthPublisher;

const MAX_CONTROL_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const MAX_CONTROL_SECRET_BYTES: usize = 1_024;
const MIN_CONTROL_SECRET_BYTES: usize = 32;
const CONTROL_REPLY_TIMEOUT: Duration = Duration::from_secs(600);

pub(crate) struct ControlRequest {
    pub(crate) batch: AutomationBatch,
    pub(crate) reply: SyncSender<AutomationRunReport>,
}

pub(crate) struct ControlService {
    address: SocketAddr,
    requests: Receiver<ControlRequest>,
    window_metrics: Arc<ControlWindowMetrics>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    batch_worker: Option<JoinHandle<()>>,
}

struct ControlWindowMetrics {
    surface_width_pixels: AtomicU32,
    surface_height_pixels: AtomicU32,
    scale_factor_bits: AtomicU64,
}

impl ControlWindowMetrics {
    fn new(surface_width_pixels: u32, surface_height_pixels: u32, scale_factor: f64) -> Self {
        Self {
            surface_width_pixels: AtomicU32::new(surface_width_pixels),
            surface_height_pixels: AtomicU32::new(surface_height_pixels),
            scale_factor_bits: AtomicU64::new(scale_factor.to_bits()),
        }
    }

    fn update(&self, surface_width_pixels: u32, surface_height_pixels: u32, scale_factor: f64) {
        self.surface_width_pixels
            .store(surface_width_pixels, Ordering::Release);
        self.surface_height_pixels
            .store(surface_height_pixels, Ordering::Release);
        self.scale_factor_bits
            .store(scale_factor.to_bits(), Ordering::Release);
    }
}

#[derive(Clone)]
pub(crate) struct ControlStatusMetadata {
    pub(crate) application_instance_id: String,
    pub(crate) renderer_backend: String,
    pub(crate) surface_width_pixels: u32,
    pub(crate) surface_height_pixels: u32,
    pub(crate) scale_factor: f64,
    pub(crate) stress_health: Arc<StressHealthPublisher>,
}

impl ControlService {
    pub(crate) fn start(
        port: u16,
        root: PathBuf,
        secret: String,
        status: ControlStatusMetadata,
    ) -> Result<Self, String> {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
            .map_err(|error| format!("could not bind the loopback control service: {error}"))?;
        let address = listener
            .local_addr()
            .map_err(|error| format!("could not inspect the control-service address: {error}"))?;
        if !address.ip().is_loopback() {
            return Err("the control service refused a non-loopback listener".to_owned());
        }
        let server = tiny_http::Server::from_listener(listener, None)
            .map_err(|error| format!("could not start the loopback control service: {error}"))?;
        let (request_sender, requests) = sync_channel(1);
        let window_metrics = Arc::new(ControlWindowMetrics::new(
            status.surface_width_pixels,
            status.surface_height_pixels,
            status.scale_factor,
        ));
        let worker_window_metrics = Arc::clone(&window_metrics);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let (batch_requests_sender, batch_requests) = sync_channel(1);
        let batch_request_sender = request_sender.clone();
        let batch_root = root.clone();
        let batch_secret = secret.clone();
        let batch_status = status.clone();
        let batch_window_metrics = Arc::clone(&window_metrics);
        let batch_stop = Arc::clone(&stop);
        let batch_worker = std::thread::Builder::new()
            .name("rgv-local-control-batches".to_owned())
            .spawn(move || {
                control_batch_server_loop(
                    &batch_requests,
                    &batch_request_sender,
                    &batch_root,
                    &batch_secret,
                    &batch_status,
                    &batch_window_metrics,
                    &batch_stop,
                );
            })
            .map_err(|error| {
                format!("could not start the control-service batch worker: {error}")
            })?;
        let worker = std::thread::Builder::new()
            .name("rgv-local-control".to_owned())
            .spawn(move || {
                control_server_loop(
                    &server,
                    &request_sender,
                    &root,
                    &secret,
                    &status,
                    &worker_window_metrics,
                    &batch_requests_sender,
                    &worker_stop,
                );
            })
            .map_err(|error| format!("could not start the control-service worker: {error}"))?;
        Ok(Self {
            address,
            requests,
            window_metrics,
            stop,
            worker: Some(worker),
            batch_worker: Some(batch_worker),
        })
    }

    pub(crate) const fn address(&self) -> SocketAddr {
        self.address
    }

    pub(crate) fn try_request(&self) -> Option<ControlRequest> {
        self.requests.try_recv().ok()
    }

    pub(crate) fn update_window_metrics(
        &self,
        surface_width_pixels: u32,
        surface_height_pixels: u32,
        scale_factor: f64,
    ) {
        self.window_metrics
            .update(surface_width_pixels, surface_height_pixels, scale_factor);
    }

    pub(crate) fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::warn!("local control-service worker panicked during shutdown");
        }
        if let Some(worker) = self.batch_worker.take()
            && worker.join().is_err()
        {
            tracing::warn!("local control-service batch worker panicked during shutdown");
        }
    }
}

impl Drop for ControlService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(crate) fn read_control_secret(path: &Path) -> Result<String, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("could not inspect control secret file: {error}"))?;
    if !metadata.is_file() {
        return Err("the control secret path is not a regular file".to_owned());
    }
    if metadata.len() > MAX_CONTROL_SECRET_BYTES as u64 {
        return Err(format!(
            "the control secret exceeds the {MAX_CONTROL_SECRET_BYTES}-byte limit"
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(
                "the control secret file must not be readable or writable by group or others"
                    .to_owned(),
            );
        }
    }
    let secret = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read control secret file: {error}"))?;
    let secret = secret.trim_end_matches(['\r', '\n']);
    if secret.len() < MIN_CONTROL_SECRET_BYTES || secret.len() > MAX_CONTROL_SECRET_BYTES {
        return Err(format!(
            "the control secret must contain {MIN_CONTROL_SECRET_BYTES}..={MAX_CONTROL_SECRET_BYTES} bytes"
        ));
    }
    if secret.chars().any(char::is_control) || secret.trim() != secret {
        return Err(
            "the control secret must be one printable line without surrounding spaces".to_owned(),
        );
    }
    Ok(secret.to_owned())
}

fn control_server_loop(
    server: &tiny_http::Server,
    request_sender: &SyncSender<ControlRequest>,
    root: &Path,
    secret: &str,
    status: &ControlStatusMetadata,
    window_metrics: &ControlWindowMetrics,
    batch_requests: &SyncSender<Request>,
    stop: &AtomicBool,
) {
    while !stop.load(Ordering::Acquire) {
        match server.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(request)) => {
                if request.method() == &Method::Post && request.url() == "/v1/batches" {
                    if !request_is_authorized(&request, secret) {
                        respond_error(
                            request,
                            401,
                            "unauthorized",
                            "The request was not authorized",
                        );
                        continue;
                    }
                    match batch_requests.try_send(request) {
                        Ok(()) => {}
                        Err(TrySendError::Full(request)) => respond_error(
                            request,
                            429,
                            "busy",
                            "Another control batch is already waiting",
                        ),
                        Err(TrySendError::Disconnected(request)) => respond_error(
                            request,
                            503,
                            "unavailable",
                            "The RGV batch service is unavailable",
                        ),
                    }
                    continue;
                }
                handle_control_request(
                    request,
                    request_sender,
                    root,
                    secret,
                    status,
                    window_metrics,
                    stop,
                );
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "local control-service receive failed");
            }
        }
    }
}

fn control_batch_server_loop(
    requests: &Receiver<Request>,
    request_sender: &SyncSender<ControlRequest>,
    root: &Path,
    secret: &str,
    status: &ControlStatusMetadata,
    window_metrics: &ControlWindowMetrics,
    stop: &AtomicBool,
) {
    while !stop.load(Ordering::Acquire) {
        match requests.recv_timeout(Duration::from_millis(100)) {
            Ok(request) => handle_control_request(
                request,
                request_sender,
                root,
                secret,
                status,
                window_metrics,
                stop,
            ),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[allow(clippy::too_many_lines)]
fn handle_control_request(
    mut request: Request,
    request_sender: &SyncSender<ControlRequest>,
    root: &Path,
    secret: &str,
    status: &ControlStatusMetadata,
    window_metrics: &ControlWindowMetrics,
    stop: &AtomicBool,
) {
    if request
        .remote_addr()
        .is_none_or(|address| !address.ip().is_loopback())
    {
        respond_error(
            request,
            403,
            "forbidden",
            "Only loopback clients are accepted",
        );
        return;
    }
    if request.method() == &Method::Get && request.url() == "/v1/status" {
        let surface_width_pixels = window_metrics.surface_width_pixels.load(Ordering::Acquire);
        let surface_height_pixels = window_metrics.surface_height_pixels.load(Ordering::Acquire);
        let scale_factor = f64::from_bits(window_metrics.scale_factor_bits.load(Ordering::Acquire));
        respond_json(
            request,
            200,
            &serde_json::json!({
                "schema_version": AUTOMATION_SCHEMA_VERSION,
                "control_schema_version": AUTOMATION_SCHEMA_VERSION,
                "application_version": env!("CARGO_PKG_VERSION"),
                "application_instance_id": status.application_instance_id,
                "process_id": std::process::id(),
                "window_count": 1,
                "active_window_ordinal": 1,
                "renderer_backend": status.renderer_backend,
                "surface_width_pixels": surface_width_pixels,
                "surface_height_pixels": surface_height_pixels,
                "scale_factor": scale_factor,
                "service": "rgv-local-control",
                "authentication": "bearer",
                "capabilities": [
                    "workspace_state",
                    "selection_state",
                    "relative_viewport"
                    ,"typed_configuration"
                    ,"review_queue"
                    ,"navigation_history"
                    ,"alignment_discovery"
                    ,"workspace_open"
                    ,"interaction_trace"
                    ,"interaction_recording"
                    ,"stress_health"
                ]
            })
            .to_string(),
        );
        return;
    }
    if request.method() == &Method::Get && request.url() == "/v1/stress/health" {
        if !request_is_authorized(&request, secret) {
            respond_error(
                request,
                401,
                "unauthorized",
                "The request was not authorized",
            );
            return;
        }
        match serde_json::to_string(&status.stress_health.snapshot()) {
            Ok(json) => respond_json(request, 200, &json),
            Err(error) => respond_error(
                request,
                500,
                "internal",
                &format!("Could not serialize stress health: {error}"),
            ),
        }
        return;
    }
    if request.method() != &Method::Post || request.url() != "/v1/batches" {
        respond_error(
            request,
            404,
            "not_found",
            "The control endpoint was not found",
        );
        return;
    }
    if !request_is_authorized(&request, secret) {
        respond_error(
            request,
            401,
            "unauthorized",
            "The request was not authorized",
        );
        return;
    }
    let content_type_is_json = request
        .headers()
        .iter()
        .find(|header| header.field.equiv("Content-Type"))
        .is_some_and(|header| {
            header
                .value
                .as_str()
                .split(';')
                .next()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
        });
    if !content_type_is_json {
        respond_error(
            request,
            415,
            "unsupported_media_type",
            "Content-Type must be application/json",
        );
        return;
    }
    let Some(body_length) = request.body_length() else {
        respond_error(
            request,
            411,
            "length_required",
            "A bounded Content-Length is required",
        );
        return;
    };
    if body_length == 0 || body_length > MAX_CONTROL_REQUEST_BYTES {
        respond_error(
            request,
            413,
            "request_too_large",
            &format!("The request must contain 1..={MAX_CONTROL_REQUEST_BYTES} bytes"),
        );
        return;
    }
    let mut body = Vec::with_capacity(body_length);
    if let Err(error) = request
        .as_reader()
        .take((MAX_CONTROL_REQUEST_BYTES + 1) as u64)
        .read_to_end(&mut body)
    {
        respond_error(
            request,
            400,
            "invalid_batch",
            &format!("Could not read JSON: {error}"),
        );
        return;
    }
    if body.len() != body_length {
        respond_error(
            request,
            400,
            "invalid_batch",
            "The body length did not match Content-Length",
        );
        return;
    }
    let Ok(body) = std::str::from_utf8(&body) else {
        respond_error(
            request,
            400,
            "invalid_batch",
            "The request is not UTF-8 JSON",
        );
        return;
    };
    let mut batch = match AutomationBatch::from_json(body) {
        Ok(batch) => batch,
        Err(error) => {
            let code = if matches!(error, AutomationError::UnsupportedVersion(_)) {
                "unsupported_version"
            } else {
                "invalid_batch"
            };
            respond_error(request, 400, code, &error.to_string());
            return;
        }
    };
    if let Err(error) = batch.resolve_paths_within(root) {
        respond_error(request, 403, "path_outside_root", &error.to_string());
        return;
    }
    let (reply, reply_receiver) = sync_channel(1);
    match request_sender.try_send(ControlRequest { batch, reply }) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            respond_error(request, 429, "busy", "Another control batch is waiting");
            return;
        }
        Err(TrySendError::Disconnected(_)) => {
            respond_error(request, 503, "unavailable", "The RGV window is unavailable");
            return;
        }
    }
    let deadline = Instant::now() + CONTROL_REPLY_TIMEOUT;
    loop {
        match reply_receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(report) => match report.to_json() {
                Ok(json) => respond_json(request, 200, &json),
                Err(error) => respond_error(
                    request,
                    500,
                    "internal",
                    &format!("Could not serialize the command result: {error}"),
                ),
            },
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => respond_error(
                request,
                503,
                "unavailable",
                "The RGV command executor became unavailable",
            ),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if stop.load(Ordering::Acquire) => {
                respond_error(request, 503, "unavailable", "RGV is shutting down");
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                respond_error(
                    request,
                    504,
                    "timeout",
                    "The control batch exceeded the ten-minute request limit",
                );
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
        }
        break;
    }
}

fn request_is_authorized(request: &Request, secret: &str) -> bool {
    let authorization = request
        .headers()
        .iter()
        .find(|header| header.field.equiv("Authorization"))
        .map(|header| header.value.as_str())
        .unwrap_or_default();
    let expected = format!("Bearer {secret}");
    constant_time_equal(authorization.as_bytes(), expected.as_bytes())
}

fn respond_error(request: Request, status: u16, code: &str, message: &str) {
    respond_json(
        request,
        status,
        &serde_json::json!({
            "schema_version": AUTOMATION_SCHEMA_VERSION,
            "succeeded": false,
            "error": {"code": code, "message": message}
        })
        .to_string(),
    );
}

fn respond_json(request: Request, status: u16, json: &str) {
    let mut response = Response::from_string(json.to_owned()).with_status_code(StatusCode(status));
    if let Ok(header) = Header::from_bytes("Content-Type", "application/json; charset=utf-8") {
        response.add_header(header);
    }
    if let Ok(header) = Header::from_bytes("Cache-Control", "no-store") {
        response.add_header(header);
    }
    if let Err(error) = request.respond(response) {
        tracing::debug!(%error, "local control response could not be delivered");
    }
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let maximum = left.len().max(right.len());
    for index in 0..maximum {
        difference |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read as _, Write as _},
        net::{Shutdown, TcpStream},
        time::Duration,
    };

    use rgv_core::{
        AutomationCommand, AutomationCommandResult, AutomationCommandStatus, AutomationRunTelemetry,
    };

    fn test_status() -> ControlStatusMetadata {
        ControlStatusMetadata {
            application_instance_id: "test-instance".to_owned(),
            renderer_backend: "test".to_owned(),
            surface_width_pixels: 800,
            surface_height_pixels: 600,
            scale_factor: 1.0,
            stress_health: Arc::new(StressHealthPublisher::new("test-instance".to_owned())),
        }
    }

    use super::*;

    const TEST_SECRET: &str = "0123456789abcdef0123456789abcdef";

    fn send_request(address: SocketAddr, request: &str) -> String {
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    fn batch_request(address: SocketAddr, secret: &str, body: &str) -> String {
        send_request(
            address,
            &format!(
                "POST /v1/batches HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {secret}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
        )
    }

    #[test]
    fn public_status_advertises_non_sensitive_control_capabilities() {
        let root = tempfile::tempdir().unwrap();
        let mut service = ControlService::start(
            0,
            root.path().to_owned(),
            TEST_SECRET.to_owned(),
            test_status(),
        )
        .unwrap();
        service.update_window_metrics(1_024, 768, 2.0);
        let response = send_request(
            service.address(),
            "GET /v1/status HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        );
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.contains("workspace_state"));
        assert!(response.contains("selection_state"));
        assert!(response.contains("relative_viewport"));
        assert!(response.contains("\"surface_width_pixels\":1024"));
        assert!(response.contains("\"surface_height_pixels\":768"));
        assert!(response.contains("\"scale_factor\":2.0"));
        assert!(!response.contains(TEST_SECRET));
        assert!(!response.contains(root.path().to_string_lossy().as_ref()));
        service.shutdown();
    }

    #[test]
    fn authenticated_loopback_batch_returns_the_structured_executor_report() {
        let root = tempfile::tempdir().unwrap();
        let mut service = ControlService::start(
            0,
            root.path().to_owned(),
            TEST_SECRET.to_owned(),
            test_status(),
        )
        .unwrap();
        assert!(service.address().ip().is_loopback());
        let body = serde_json::json!({
            "schema_version": AUTOMATION_SCHEMA_VERSION,
            "commands": [{"command": "goto", "loci": ["chr1:1-10"]}]
        })
        .to_string();
        let address = service.address();
        let client = std::thread::spawn(move || batch_request(address, TEST_SECRET, &body));
        let request = loop {
            if let Some(request) = service.try_request() {
                break request;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(matches!(
            request.batch.commands.as_slice(),
            [AutomationCommand::Goto { .. }]
        ));
        request
            .reply
            .send(AutomationRunReport {
                schema_version: AUTOMATION_SCHEMA_VERSION,
                succeeded: true,
                results: vec![AutomationCommandResult {
                    command_index: 0,
                    command: "goto".to_owned(),
                    status: AutomationCommandStatus::Succeeded,
                    error_code: None,
                    message: "Navigated".to_owned(),
                    data: serde_json::json!({"settled": true}),
                    elapsed_ms: 0.0,
                }],
                telemetry: AutomationRunTelemetry::default(),
            })
            .unwrap();
        let response = client.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.contains("\"succeeded\": true"));
        service.shutdown();
    }

    #[test]
    fn missing_secret_is_rejected_without_reaching_the_window() {
        let root = tempfile::tempdir().unwrap();
        let mut service = ControlService::start(
            0,
            root.path().to_owned(),
            TEST_SECRET.to_owned(),
            test_status(),
        )
        .unwrap();
        let body = r#"{"schema_version":1,"commands":[{"command":"exit"}]}"#;
        let response = batch_request(service.address(), "wrong", body);
        assert!(response.starts_with("HTTP/1.1 401"));
        assert!(service.try_request().is_none());
        service.shutdown();
    }

    #[test]
    fn stress_health_is_authenticated_and_does_not_enter_the_ui_queue() {
        let root = tempfile::tempdir().unwrap();
        let status = test_status();
        status.stress_health.note_ui_heartbeat();
        status.stress_health.note_native_input();
        let mut service =
            ControlService::start(0, root.path().to_owned(), TEST_SECRET.to_owned(), status)
                .unwrap();

        let denied = send_request(
            service.address(),
            "GET /v1/stress/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        );
        assert!(denied.starts_with("HTTP/1.1 401"));
        let accepted = send_request(
            service.address(),
            &format!(
                "GET /v1/stress/health HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {TEST_SECRET}\r\nConnection: close\r\n\r\n"
            ),
        );
        assert!(accepted.starts_with("HTTP/1.1 200"));
        assert!(accepted.contains("\"ui_heartbeat_sequence\":1"));
        assert!(accepted.contains("\"native_input_sequence\":1"));
        assert!(service.try_request().is_none());
        service.shutdown();
    }

    #[test]
    fn stress_health_remains_available_while_a_batch_waits_for_the_ui() {
        let root = tempfile::tempdir().unwrap();
        let mut service = ControlService::start(
            0,
            root.path().to_owned(),
            TEST_SECRET.to_owned(),
            test_status(),
        )
        .unwrap();
        let body = serde_json::json!({
            "schema_version": AUTOMATION_SCHEMA_VERSION,
            "commands": [{"command": "wait_until_settled", "timeout_millis": 30_000}]
        })
        .to_string();
        let address = service.address();
        let batch_client = std::thread::spawn(move || batch_request(address, TEST_SECRET, &body));
        let pending = loop {
            if let Some(request) = service.try_request() {
                break request;
            }
            std::thread::sleep(Duration::from_millis(5));
        };

        let health = send_request(
            service.address(),
            &format!(
                "GET /v1/stress/health HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {TEST_SECRET}\r\nConnection: close\r\n\r\n"
            ),
        );
        assert!(health.starts_with("HTTP/1.1 200"));

        pending
            .reply
            .send(AutomationRunReport {
                schema_version: AUTOMATION_SCHEMA_VERSION,
                succeeded: true,
                results: vec![AutomationCommandResult {
                    command_index: 0,
                    command: "wait_until_settled".to_owned(),
                    status: AutomationCommandStatus::Succeeded,
                    error_code: None,
                    message: "Settled".to_owned(),
                    data: serde_json::json!({"settled": true}),
                    elapsed_ms: 0.0,
                }],
                telemetry: AutomationRunTelemetry::default(),
            })
            .unwrap();
        assert!(batch_client.join().unwrap().starts_with("HTTP/1.1 200"));
        service.shutdown();
    }

    #[test]
    fn constant_time_comparison_requires_every_byte_and_length() {
        assert!(constant_time_equal(b"same", b"same"));
        assert!(!constant_time_equal(b"same", b"some"));
        assert!(!constant_time_equal(b"same", b"same-more"));
    }

    #[test]
    fn control_secret_is_bounded_and_loaded_without_its_trailing_newline() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("control.secret");
        std::fs::write(&path, format!("{TEST_SECRET}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(read_control_secret(&path).unwrap(), TEST_SECRET);

        std::fs::write(&path, "too-short\n").unwrap();
        assert!(read_control_secret(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn control_secret_rejects_group_or_world_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("control.secret");
        std::fs::write(&path, TEST_SECRET).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            read_control_secret(&path)
                .unwrap_err()
                .contains("group or others")
        );
    }
}
