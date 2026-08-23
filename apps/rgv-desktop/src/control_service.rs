use std::{
    io::Read as _,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use rgv_core::{AUTOMATION_SCHEMA_VERSION, AutomationBatch, AutomationError, AutomationRunReport};
use tiny_http::{Header, Method, Request, Response, StatusCode};

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
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ControlService {
    pub(crate) fn start(port: u16, root: PathBuf, secret: String) -> Result<Self, String> {
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
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = std::thread::Builder::new()
            .name("rgv-local-control".to_owned())
            .spawn(move || {
                control_server_loop(&server, &request_sender, &root, &secret, &worker_stop);
            })
            .map_err(|error| format!("could not start the control-service worker: {error}"))?;
        Ok(Self {
            address,
            requests,
            stop,
            worker: Some(worker),
        })
    }

    pub(crate) const fn address(&self) -> SocketAddr {
        self.address
    }

    pub(crate) fn try_request(&self) -> Option<ControlRequest> {
        self.requests.try_recv().ok()
    }

    pub(crate) fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::warn!("local control-service worker panicked during shutdown");
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
    stop: &AtomicBool,
) {
    while !stop.load(Ordering::Acquire) {
        match server.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(request)) => {
                handle_control_request(request, request_sender, root, secret, stop);
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "local control-service receive failed");
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
fn handle_control_request(
    mut request: Request,
    request_sender: &SyncSender<ControlRequest>,
    root: &Path,
    secret: &str,
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
        respond_json(
            request,
            200,
            &serde_json::json!({
                "schema_version": AUTOMATION_SCHEMA_VERSION,
                "service": "rgv-local-control",
                "authentication": "bearer"
            })
            .to_string(),
        );
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
    let authorization = request
        .headers()
        .iter()
        .find(|header| header.field.equiv("Authorization"))
        .map(|header| header.value.as_str())
        .unwrap_or_default();
    let expected = format!("Bearer {secret}");
    if !constant_time_equal(authorization.as_bytes(), expected.as_bytes()) {
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

    use super::*;

    const TEST_SECRET: &str = "0123456789abcdef0123456789abcdef";

    fn send_request(address: SocketAddr, request: &str) -> String {
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
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
    fn authenticated_loopback_batch_returns_the_structured_executor_report() {
        let root = tempfile::tempdir().unwrap();
        let mut service =
            ControlService::start(0, root.path().to_owned(), TEST_SECRET.to_owned()).unwrap();
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
        let mut service =
            ControlService::start(0, root.path().to_owned(), TEST_SECRET.to_owned()).unwrap();
        let body = r#"{"schema_version":1,"commands":[{"command":"exit"}]}"#;
        let response = batch_request(service.address(), "wrong", body);
        assert!(response.starts_with("HTTP/1.1 401"));
        assert!(service.try_request().is_none());
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
