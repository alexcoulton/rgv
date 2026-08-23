use std::{
    collections::BTreeMap,
    fs,
    io::{Read as _, Write as _},
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use clap::Parser;
use rgv_core::GenomicInterval;
use rgv_hts::{
    AlignmentOpenOptions, AlignmentSource, ProductionAlignmentSource, QueryDetail, QueryOptions,
};
use rgv_validation_runner::{FixtureEndpointDocument, read_json};

#[derive(Debug, Parser)]
#[command(about = "Qualify every controlled RGV loopback fixture-server behavior")]
struct Arguments {
    #[arg(long, default_value = "benchmark-data/manual-suite")]
    suite: PathBuf,
    #[arg(long, default_value_t = 250)]
    slow_ms: u64,
}

struct ChildGuard(Option<Child>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<()> {
    let arguments = Arguments::parse();
    ensure!(arguments.slow_ms >= 100, "--slow-ms must be at least 100");
    let server_binary = std::env::current_exe()?
        .parent()
        .context("qualification binary has no parent directory")?
        .join(executable_name("rgv-fixture-server"));
    ensure!(
        server_binary.is_file(),
        "{} is missing; build both release binaries with `cargo build --release -p rgv-validation-runner --bins`",
        server_binary.display()
    );
    let endpoint_path = temporary_endpoint_path();
    let child = Command::new(&server_binary)
        .arg("--suite")
        .arg(&arguments.suite)
        .arg("--write-endpoint")
        .arg(&endpoint_path)
        .arg("--max-requests")
        .arg("0")
        .arg("--slow-ms")
        .arg(arguments.slow_ms.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to start {}", server_binary.display()))?;
    let mut child = ChildGuard(Some(child));
    wait_for_file(&endpoint_path, Duration::from_secs(10))?;
    let endpoint: FixtureEndpointDocument = read_json(&endpoint_path)?;
    let authority = endpoint
        .base_url
        .strip_prefix("http://")
        .context("fixture endpoint is not loopback HTTP")?;
    let bcf_path = fixture_path(&endpoint, "manual-cohort-bcf")?;
    let index_path = format!("{bcf_path}.csi");

    let head = request(authority, "HEAD", &bcf_path, &[])?;
    ensure!(head.status == 200, "healthy HEAD returned {}", head.status);
    let etag = required_header(&head, "etag")?.to_owned();
    ensure!(
        required_header(&head, "accept-ranges")? == "bytes",
        "healthy HEAD did not advertise ranges"
    );
    let range = request(
        authority,
        "GET",
        &format!("{bcf_path}?signature=redacted-test"),
        &[("Range", "bytes=0-31")],
    )?;
    ensure!(
        range.status == 206
            && range.body.len() == 32
            && required_header(&range, "content-range")?.starts_with("bytes 0-31/"),
        "healthy bounded range response changed"
    );
    let healthy_body = request(authority, "GET", &bcf_path, &[])?.body;
    let conditional = request(authority, "GET", &bcf_path, &[("If-None-Match", &etag)])?;
    ensure!(
        conditional.status == 304 && conditional.body.is_empty(),
        "matching If-None-Match did not return an empty 304"
    );
    let adjacent = request(authority, "GET", &index_path, &[("Range", "bytes=0-15")])?;
    ensure!(
        adjacent.status == 206 && adjacent.body.len() == 16,
        "adjacent BCF index routing failed"
    );
    let mut remote_alignment = ProductionAlignmentSource::open(
        endpoint
            .fixture_urls
            .get("manual-rna-a-bam")
            .context("endpoint has no focused RNA BAM")?,
        &AlignmentOpenOptions {
            remote_index_url: Some(
                endpoint
                    .fixture_urls
                    .get("manual-rna-a-bai")
                    .context("endpoint has no focused RNA BAI")?
                    .clone(),
            ),
            ..AlignmentOpenOptions::default()
        },
    )?;
    let contig = remote_alignment
        .reference_dictionary()
        .id_by_name("chrManual")
        .context("remote focused RNA BAM has no chrManual")?;
    let remote_query = remote_alignment.query(
        GenomicInterval::new(contig, 19_900, 21_100)?,
        QueryOptions {
            detail: QueryDetail::Full,
            ..QueryOptions::default()
        },
        &|| true,
    )?;
    ensure!(
        remote_query.batch.len() == 10 && remote_query.junctions.provenance.spliced_records == 9,
        "production remote BAM path did not preserve focused RNA truth"
    );

    change_profile(authority, &endpoint.control_secret, "redirect")?;
    let redirected = request(
        authority,
        "GET",
        &format!("{bcf_path}?signature=redacted-test"),
        &[],
    )?;
    let location = required_header(&redirected, "location")?;
    ensure!(
        redirected.status == 307
            && location.contains("signature=redacted-test")
            && location.contains("redirected=1"),
        "redirect did not preserve the signed query"
    );
    ensure!(
        request(authority, "GET", location, &[])?.status == 200,
        "redirect target did not recover"
    );

    change_profile(authority, &endpoint.control_secret, "auth-required")?;
    ensure!(
        request(authority, "GET", &bcf_path, &[])?.status == 401,
        "auth-required accepted an unauthenticated request"
    );
    let authorization = format!("Bearer {}", endpoint.bearer_token);
    ensure!(
        request(
            authority,
            "GET",
            &bcf_path,
            &[("Authorization", &authorization)],
        )?
        .status
            == 200,
        "auth-required rejected the runtime bearer token"
    );

    change_profile(authority, &endpoint.control_secret, "fail-first-n")?;
    ensure!(
        request(authority, "GET", &bcf_path, &[])?.status == 503
            && request(authority, "GET", &bcf_path, &[])?.status == 200,
        "fail-first profile did not fail once and recover"
    );
    change_profile(authority, &endpoint.control_secret, "no-range")?;
    let no_range = request(authority, "GET", &bcf_path, &[("Range", "bytes=0-31")])?;
    ensure!(
        no_range.status == 200 && !no_range.headers.contains_key("accept-ranges"),
        "no-range profile still advertised or returned a range"
    );

    change_profile(authority, &endpoint.control_secret, "changed-etag")?;
    let first_etag =
        required_header(&request(authority, "HEAD", &bcf_path, &[])?, "etag")?.to_owned();
    let second_etag =
        required_header(&request(authority, "HEAD", &bcf_path, &[])?, "etag")?.to_owned();
    ensure!(
        first_etag != second_etag,
        "changed-etag profile kept a stable validator"
    );

    change_profile(authority, &endpoint.control_secret, "corrupt")?;
    let corrupt = request(authority, "GET", &bcf_path, &[])?.body;
    ensure!(
        corrupt.len() == healthy_body.len() && corrupt != healthy_body,
        "corrupt profile did not change exactly the served object content"
    );
    change_profile(authority, &endpoint.control_secret, "short-body")?;
    let short = request(authority, "GET", &bcf_path, &[("Range", "bytes=0-31")])?;
    ensure!(
        short.status == 206
            && short.body.len() == 31
            && required_header(&short, "content-range")?.starts_with("bytes 0-31/"),
        "short-body profile did not expose a one-byte truncation"
    );
    change_profile(authority, &endpoint.control_secret, "invalid-content-type")?;
    ensure!(
        required_header(&request(authority, "GET", &bcf_path, &[])?, "content-type")?
            == "image/jpeg",
        "invalid-content-type profile did not declare the controlled wrong type"
    );
    change_profile(authority, &endpoint.control_secret, "outage")?;
    ensure!(
        request(authority, "GET", &bcf_path, &[])?.status == 503,
        "outage profile returned data"
    );
    change_profile(authority, &endpoint.control_secret, "slow")?;
    let started = Instant::now();
    ensure!(
        request(authority, "GET", &bcf_path, &[])?.status == 200
            && started.elapsed() >= Duration::from_millis(arguments.slow_ms),
        "slow profile was not observably delayed"
    );
    change_profile(authority, &endpoint.control_secret, "healthy")?;
    ensure!(
        request(authority, "GET", &bcf_path, &[])?.body == healthy_body,
        "healthy profile did not recover the original object"
    );
    ensure!(
        request(authority, "GET", "/fixture/not-declared/file.bam", &[])?.status == 404,
        "server exposed an undeclared fixture route"
    );
    let shutdown = request(
        authority,
        "POST",
        "/__shutdown",
        &[("X-RGV-Fixture-Control", &endpoint.control_secret)],
    )?;
    ensure!(
        shutdown.status == 200,
        "fixture server did not accept clean shutdown"
    );

    let status = child
        .0
        .as_mut()
        .context("fixture server process was unavailable")?
        .wait()?;
    child.0 = None;
    ensure!(status.success(), "fixture server exited unsuccessfully");
    fs::remove_file(&endpoint_path)?;
    println!(
        "fixture server qualification passed a production remote BAM query plus bounded BCF/index ranges, conditional, auth, redirect, retry, validator, corruption, truncation, outage, slow, recovery, and clean shutdown cases"
    );
    Ok(())
}

fn executable_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

fn temporary_endpoint_path() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "rgv-fixture-endpoint-{}-{nonce}.json",
        std::process::id()
    ))
}

fn wait_for_file(path: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.is_file() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
    anyhow::bail!("fixture server did not write {}", path.display())
}

fn fixture_path(endpoint: &FixtureEndpointDocument, id: &str) -> Result<String> {
    let url = endpoint
        .fixture_urls
        .get(id)
        .with_context(|| format!("endpoint has no fixture {id}"))?;
    Ok(url
        .strip_prefix(&endpoint.base_url)
        .context("fixture URL is outside its endpoint base")?
        .to_owned())
}

fn change_profile(authority: &str, secret: &str, profile: &str) -> Result<()> {
    let response = request(
        authority,
        "POST",
        &format!("/__profile/{profile}"),
        &[("X-RGV-Fixture-Control", secret)],
    )?;
    ensure!(response.status == 200, "could not select profile {profile}");
    Ok(())
}

fn request(
    authority: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> Result<HttpResponse> {
    let mut stream = TcpStream::connect(authority)
        .with_context(|| format!("failed to connect to loopback fixture server at {authority}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(35)))?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\nContent-Length: 0\r\n"
    )?;
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    write!(stream, "\r\n")?;
    stream.flush()?;
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes)?;
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("HTTP response had no header terminator")?;
    let header_text = std::str::from_utf8(&bytes[..split])?;
    let mut lines = header_text.lines();
    let status = lines
        .next()
        .context("HTTP response had no status line")?
        .split_whitespace()
        .nth(1)
        .context("HTTP response had no status code")?
        .parse()?;
    let response_headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Ok(HttpResponse {
        status,
        headers: response_headers,
        body: bytes[split + 4..].to_vec(),
    })
}

fn required_header<'a>(response: &'a HttpResponse, name: &str) -> Result<&'a str> {
    response
        .headers
        .get(name)
        .map(String::as_str)
        .with_context(|| format!("HTTP {} response has no {name} header", response.status))
}
