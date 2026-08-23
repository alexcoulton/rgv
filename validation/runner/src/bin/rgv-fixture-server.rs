use std::{
    collections::BTreeMap,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    process,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use clap::{Parser, ValueEnum};
use rgv_validation_runner::{
    DEFAULT_SUITE_ROOT, FixtureEndpointDocument, FixtureManifest, read_json, sha256_bytes,
    workspace_root, write_json,
};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[clap(rename_all = "kebab-case")]
enum Profile {
    Healthy,
    AuthRequired,
    Redirect,
    FailFirstN,
    Outage,
    Slow,
    NoRange,
    ShortBody,
    ChangedEtag,
    Corrupt,
    InvalidContentType,
}

impl Profile {
    const fn name(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::AuthRequired => "auth-required",
            Self::Redirect => "redirect",
            Self::FailFirstN => "fail-first-n",
            Self::Outage => "outage",
            Self::Slow => "slow",
            Self::NoRange => "no-range",
            Self::ShortBody => "short-body",
            Self::ChangedEtag => "changed-etag",
            Self::Corrupt => "corrupt",
            Self::InvalidContentType => "invalid-content-type",
        }
    }
}

#[derive(Debug, Parser)]
#[command(about = "Controlled loopback HTTP source and failure lab for RGV manual fixtures")]
struct Arguments {
    #[arg(long, default_value = DEFAULT_SUITE_ROOT)]
    suite: PathBuf,
    #[arg(long, default_value = "127.0.0.1:0")]
    bind: String,
    #[arg(long, value_enum, default_value_t = Profile::Healthy)]
    profile: Profile,
    #[arg(long, default_value_t = 1)]
    fail_first: u64,
    #[arg(long, default_value_t = 250)]
    slow_ms: u64,
    #[arg(long)]
    write_endpoint: Option<PathBuf>,
    /// Exit after this many requests. Zero serves until externally stopped.
    #[arg(long, default_value_t = 0)]
    max_requests: u64,
}

struct State {
    fixtures: BTreeMap<String, PathBuf>,
    profile: Profile,
    bearer_token: String,
    control_secret: String,
    fail_first: u64,
    slow_ms: u64,
    requests: u64,
    data_requests: u64,
    shutdown_requested: bool,
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    ensure!(arguments.fail_first > 0, "--fail-first must be positive");
    ensure!(
        arguments.slow_ms <= 30_000,
        "--slow-ms must not exceed 30 seconds"
    );
    let root = workspace_root();
    let suite = root.join(&arguments.suite);
    let manifest: FixtureManifest = read_json(&suite.join("manifest.json"))?;
    let fixtures = declared_fixtures(&root, &manifest)?;
    let bearer_token = runtime_secret("bearer");
    let control_secret = runtime_secret("control");
    let server =
        Server::http(&arguments.bind).map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let address = server
        .server_addr()
        .to_ip()
        .context("fixture server did not receive an IP address")?;
    ensure!(
        address.ip().is_loopback(),
        "fixture server must bind to loopback"
    );
    let base_url = format!("http://{address}");
    let endpoint = FixtureEndpointDocument {
        schema_version: 1,
        process_id: process::id(),
        base_url: base_url.clone(),
        profile: arguments.profile.name().to_owned(),
        bearer_token: bearer_token.clone(),
        control_secret: control_secret.clone(),
        fixture_urls: fixtures
            .iter()
            .map(|(id, path)| {
                let filename = path
                    .file_name()
                    .expect("declared fixture paths name a file")
                    .to_string_lossy();
                (id.clone(), format!("{base_url}/fixture/{id}/{filename}"))
            })
            .collect(),
    };
    if let Some(path) = &arguments.write_endpoint {
        let absolute = if path.is_absolute() {
            path.clone()
        } else {
            std::env::current_dir()?.join(path)
        };
        ensure!(
            !absolute.starts_with(&root),
            "runtime endpoint credentials may not be written inside the workspace"
        );
        write_json(&absolute, &endpoint)?;
        println!("wrote runtime endpoint {}", absolute.display());
    }
    println!("{}", serde_json::to_string(&endpoint)?);

    let mut state = State {
        fixtures,
        profile: arguments.profile,
        bearer_token,
        control_secret,
        fail_first: arguments.fail_first,
        slow_ms: arguments.slow_ms,
        requests: 0,
        data_requests: 0,
        shutdown_requested: false,
    };
    for request in server.incoming_requests() {
        state.requests = state.requests.saturating_add(1);
        handle_request(request, &mut state)?;
        if state.shutdown_requested
            || (arguments.max_requests > 0 && state.requests >= arguments.max_requests)
        {
            break;
        }
    }
    eprintln!(
        "fixture server stopped after {} requests ({} data requests)",
        state.requests, state.data_requests
    );
    Ok(())
}

fn declared_fixtures(root: &Path, manifest: &FixtureManifest) -> Result<BTreeMap<String, PathBuf>> {
    let mut fixtures = BTreeMap::new();
    for (id, relative) in manifest
        .artifacts
        .iter()
        .map(|artifact| (&artifact.id, &artifact.path))
        .chain(
            manifest
                .dependencies
                .iter()
                .map(|dependency| (&dependency.id, &dependency.path)),
        )
    {
        let path = root.join(relative);
        ensure!(
            path.starts_with(root),
            "declared fixture escaped the workspace"
        );
        ensure!(path.is_file(), "declared fixture {id} is missing");
        ensure!(
            fixtures.insert(id.clone(), path).is_none(),
            "duplicate fixture ID {id}"
        );
    }
    Ok(fixtures)
}

fn runtime_secret(purpose: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    sha256_bytes(format!("rgv-fixture-{purpose}-{}-{now}", process::id()).as_bytes())
}

#[allow(clippy::too_many_lines)]
fn handle_request(request: Request, state: &mut State) -> Result<()> {
    let raw_url = request.url().to_owned();
    let (path, query) = raw_url.split_once('?').unwrap_or((&raw_url, ""));
    if let Some(profile) = path.strip_prefix("/__profile/") {
        return handle_profile_change(request, state, profile);
    }
    if path == "/__shutdown" {
        return handle_shutdown(request, state);
    }
    let Some(route) = path.strip_prefix("/fixture/") else {
        request.respond(text_response(StatusCode(404), "unknown fixture route\n"))?;
        return Ok(());
    };
    let (route_id, requested_name) = route.split_once('/').unwrap_or((route, ""));
    let id = adjacent_index_fixture_id(route_id, requested_name).unwrap_or(route_id);
    let Some(file_path) = state.fixtures.get(id).cloned() else {
        request.respond(text_response(StatusCode(404), "unknown fixture ID\n"))?;
        return Ok(());
    };
    state.data_requests = state.data_requests.saturating_add(1);
    eprintln!(
        "request={} method={:?} fixture={} query_keys={}",
        state.data_requests,
        request.method(),
        id,
        redacted_query_keys(query)
    );

    if state.profile == Profile::AuthRequired && !authorized(&request, &state.bearer_token) {
        let response = text_response(StatusCode(401), "bearer token required\n")
            .with_header(header("WWW-Authenticate", "Bearer realm=\"rgv-fixture\""));
        request.respond(response)?;
        return Ok(());
    }
    if state.profile == Profile::Outage
        || (state.profile == Profile::FailFirstN && state.data_requests <= state.fail_first)
    {
        request.respond(text_response(
            StatusCode(503),
            "controlled fixture outage\n",
        ))?;
        return Ok(());
    }
    if state.profile == Profile::Redirect && !query.split('&').any(|part| part == "redirected=1") {
        let separator = if query.is_empty() { "?" } else { "&" };
        let location = format!("{path}?{query}{separator}redirected=1");
        request
            .respond(Response::empty(StatusCode(307)).with_header(header("Location", &location)))?;
        return Ok(());
    }
    if state.profile == Profile::Slow {
        std::thread::sleep(Duration::from_millis(state.slow_ms));
    }

    let mut bytes = fs::read(&file_path)
        .with_context(|| format!("failed to read served fixture {}", file_path.display()))?;
    if state.profile == Profile::Corrupt && !bytes.is_empty() {
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0xff;
    }
    let object_length = bytes.len();
    let etag = if state.profile == Profile::ChangedEtag && state.data_requests > 1 {
        format!("\"{}-changed\"", sha256_bytes(&bytes))
    } else {
        format!("\"{}\"", sha256_bytes(&bytes))
    };
    let last_modified = "Sat, 23 Aug 2026 00:00:00 GMT";
    let has_etag_condition = request
        .headers()
        .iter()
        .any(|candidate| candidate.field.equiv("If-None-Match"));
    let not_modified = request.headers().iter().any(|candidate| {
        candidate.field.equiv("If-None-Match") && candidate.value.as_str() == etag
    }) || (!has_etag_condition
        && request.headers().iter().any(|candidate| {
            candidate.field.equiv("If-Modified-Since") && candidate.value.as_str() == last_modified
        }));
    if not_modified {
        request.respond(
            Response::empty(StatusCode(304))
                .with_header(header("ETag", &etag))
                .with_header(header("Last-Modified", last_modified)),
        )?;
        return Ok(());
    }
    if request.method() == &Method::Head {
        let mut response = Response::empty(StatusCode(200))
            .with_header(header("Content-Length", &object_length.to_string()))
            .with_header(header("ETag", &etag))
            .with_header(header("Last-Modified", last_modified));
        if state.profile != Profile::NoRange {
            response = response.with_header(header("Accept-Ranges", "bytes"));
        }
        if state.profile == Profile::InvalidContentType {
            response = response.with_header(header("Content-Type", "image/jpeg"));
        }
        request.respond(response)?;
        return Ok(());
    }
    if request.method() != &Method::Get {
        request.respond(text_response(StatusCode(405), "GET and HEAD only\n"))?;
        return Ok(());
    }

    let requested_range = request
        .headers()
        .iter()
        .find(|candidate| candidate.field.equiv("Range"))
        .and_then(|candidate| parse_range(candidate.value.as_str(), object_length));
    let (status, start, end) = if state.profile == Profile::NoRange {
        (StatusCode(200), 0, object_length.saturating_sub(1))
    } else if let Some((start, end)) = requested_range {
        (StatusCode(206), start, end)
    } else {
        (StatusCode(200), 0, object_length.saturating_sub(1))
    };
    let mut body = if object_length == 0 {
        Vec::new()
    } else {
        bytes[start..=end].to_vec()
    };
    if state.profile == Profile::ShortBody && !body.is_empty() {
        body.truncate(body.len().saturating_sub(1).max(1));
    }
    let mut response = Response::new(
        status,
        Vec::new(),
        Cursor::new(body.clone()),
        Some(body.len()),
        None,
    )
    .with_header(header("ETag", &etag))
    .with_header(header("Last-Modified", last_modified));
    if state.profile != Profile::NoRange {
        response = response.with_header(header("Accept-Ranges", "bytes"));
    }
    if status == StatusCode(206) {
        response = response.with_header(header(
            "Content-Range",
            &format!("bytes {start}-{end}/{object_length}"),
        ));
    }
    if state.profile == Profile::InvalidContentType {
        response = response.with_header(header("Content-Type", "image/jpeg"));
    }
    request.respond(response)?;
    Ok(())
}

fn adjacent_index_fixture_id<'a>(route_id: &'a str, requested_name: &str) -> Option<&'a str> {
    if has_extension(requested_name, "csi") {
        return match route_id {
            "manual-cohort-bcf" => Some("manual-cohort-csi"),
            "dep-demo-variants.bcf" => Some("dep-demo-variants.bcf.csi"),
            _ => None,
        };
    }
    if has_extension(requested_name, "tbi") && route_id == "manual-cohort-vcf" {
        return Some("manual-cohort-tbi");
    }
    if has_extension(requested_name, "bai") && route_id == "dep-mixed-bam" {
        return Some("dep-mixed-bai");
    }
    if has_extension(requested_name, "crai") && route_id == "manual-reference-cram" {
        return Some("manual-reference-crai");
    }
    if has_extension(requested_name, "fai") {
        return match route_id {
            "manual-reference-fasta" => Some("manual-reference-fai"),
            "dep-variants-fasta" => Some("dep-variants-fai"),
            _ => None,
        };
    }
    None
}

fn has_extension(name: &str, expected: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

fn handle_profile_change(request: Request, state: &mut State, profile: &str) -> Result<()> {
    if request.method() != &Method::Post
        || !request.headers().iter().any(|candidate| {
            candidate.field.equiv("X-RGV-Fixture-Control")
                && candidate.value.as_str() == state.control_secret
        })
    {
        request.respond(text_response(StatusCode(403), "control secret required\n"))?;
        return Ok(());
    }
    let Some(profile) = parse_profile(profile) else {
        request.respond(text_response(StatusCode(400), "unknown fixture profile\n"))?;
        return Ok(());
    };
    state.profile = profile;
    state.data_requests = 0;
    request.respond(text_response(StatusCode(200), "profile changed\n"))?;
    Ok(())
}

fn handle_shutdown(request: Request, state: &mut State) -> Result<()> {
    if request.method() != &Method::Post
        || !request.headers().iter().any(|candidate| {
            candidate.field.equiv("X-RGV-Fixture-Control")
                && candidate.value.as_str() == state.control_secret
        })
    {
        request.respond(text_response(StatusCode(403), "control secret required\n"))?;
        return Ok(());
    }
    state.shutdown_requested = true;
    request.respond(text_response(StatusCode(200), "fixture server stopping\n"))?;
    Ok(())
}

fn parse_profile(value: &str) -> Option<Profile> {
    [
        Profile::Healthy,
        Profile::AuthRequired,
        Profile::Redirect,
        Profile::FailFirstN,
        Profile::Outage,
        Profile::Slow,
        Profile::NoRange,
        Profile::ShortBody,
        Profile::ChangedEtag,
        Profile::Corrupt,
        Profile::InvalidContentType,
    ]
    .into_iter()
    .find(|profile| profile.name() == value)
}

fn authorized(request: &Request, token: &str) -> bool {
    request.headers().iter().any(|candidate| {
        candidate.field.equiv("Authorization")
            && candidate.value.as_str() == format!("Bearer {token}")
    })
}

fn redacted_query_keys(query: &str) -> String {
    if query.is_empty() {
        return "-".to_owned();
    }
    query
        .split('&')
        .filter_map(|part| {
            part.split_once('=')
                .map_or(Some(part), |(key, _)| Some(key))
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_range(value: &str, length: usize) -> Option<(usize, usize)> {
    let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
    let start = start.parse::<usize>().ok()?;
    let end = if end.is_empty() {
        length.checked_sub(1)?
    } else {
        end.parse::<usize>().ok()?.min(length.checked_sub(1)?)
    };
    (start <= end && start < length).then_some((start, end))
}

fn text_response(status: StatusCode, text: &str) -> Response<Cursor<Vec<u8>>> {
    Response::from_data(text.as_bytes().to_vec()).with_status_code(status)
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("static HTTP header is valid")
}
