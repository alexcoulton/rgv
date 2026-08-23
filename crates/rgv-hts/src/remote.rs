use std::{
    fmt, fs,
    io::{self, Read, Seek, SeekFrom},
    num::NonZeroUsize,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use lru::LruCache;
use reqwest::{
    blocking::{Client, RequestBuilder},
    header::{
        AUTHORIZATION, CONTENT_LENGTH, CONTENT_RANGE, ETAG, HeaderName, HeaderValue, IF_RANGE,
        LAST_MODIFIED, RANGE,
    },
    redirect::Policy,
};
use tempfile::TempDir;
use thiserror::Error;
use url::Url;

#[derive(Debug)]
pub struct RemoteHttpError {
    status: Option<reqwest::StatusCode>,
    category: &'static str,
}

impl RemoteHttpError {
    pub const fn status(&self) -> Option<reqwest::StatusCode> {
        self.status
    }
}

impl fmt::Display for RemoteHttpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.status {
            Some(status) => write!(formatter, "remote HTTP request returned {status}"),
            None => write!(formatter, "remote HTTP request {}", self.category),
        }
    }
}

impl std::error::Error for RemoteHttpError {}

impl From<reqwest::Error> for RemoteHttpError {
    fn from(error: reqwest::Error) -> Self {
        let category = if error.is_timeout() {
            "timed out"
        } else if error.is_connect() {
            "could not connect"
        } else if error.is_redirect() {
            "failed while following a redirect"
        } else if error.is_body() || error.is_decode() {
            "returned an invalid body"
        } else {
            "failed"
        };
        Self {
            status: error.status(),
            category,
        }
    }
}

#[derive(Clone)]
pub enum RemoteAuthentication {
    None,
    Bearer(String),
    Header {
        name: HeaderName,
        value: HeaderValue,
    },
}

impl fmt::Debug for RemoteAuthentication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("None"),
            Self::Bearer(_) => formatter.write_str("Bearer([redacted])"),
            Self::Header { name, .. } => formatter
                .debug_struct("Header")
                .field("name", name)
                .field("value", &"[redacted]")
                .finish(),
        }
    }
}

impl RemoteAuthentication {
    fn apply(&self, request: RequestBuilder) -> RequestBuilder {
        match self {
            Self::None => request,
            Self::Bearer(token) => request.header(AUTHORIZATION, format!("Bearer {token}")),
            Self::Header { name, value } => request.header(name, value),
        }
    }
}

#[derive(Clone, Debug)]
pub struct HttpRangeConfig {
    pub chunk_size: NonZeroUsize,
    pub maximum_cache_bytes: NonZeroUsize,
    pub minimum_coalesced_chunks: NonZeroUsize,
    pub maximum_coalesced_chunks: NonZeroUsize,
    pub authentication: RemoteAuthentication,
    pub request_timeout: Duration,
    pub maximum_retries: usize,
}

impl Default for HttpRangeConfig {
    fn default() -> Self {
        Self {
            chunk_size: NonZeroUsize::new(256 * 1024).unwrap(),
            maximum_cache_bytes: NonZeroUsize::new(64 * 1024 * 1024).unwrap(),
            minimum_coalesced_chunks: NonZeroUsize::MIN,
            maximum_coalesced_chunks: NonZeroUsize::new(4).unwrap(),
            authentication: RemoteAuthentication::None,
            request_timeout: Duration::from_secs(30),
            maximum_retries: 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RemoteIoStats {
    pub requests: u64,
    pub response_bytes: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub evictions: u64,
    pub retries: u64,
    pub cancellations: u64,
}

pub type RemoteCancellation = Arc<dyn Fn() -> bool + Send + Sync>;

#[derive(Clone, Debug)]
struct Validator {
    etag: Option<HeaderValue>,
    last_modified: Option<HeaderValue>,
}

impl Validator {
    fn if_range(&self) -> Option<HeaderValue> {
        self.etag.clone().or_else(|| self.last_modified.clone())
    }
}

#[derive(Debug)]
struct CachedBlock {
    path: PathBuf,
}

pub struct HttpRangeReader {
    client: Client,
    url: Url,
    length: u64,
    position: u64,
    config: HttpRangeConfig,
    validator: Validator,
    cache_directory: TempDir,
    blocks: LruCache<u64, CachedBlock>,
    current_block: Option<(u64, Vec<u8>)>,
    stats: RemoteIoStats,
    redirect_count: Arc<AtomicU64>,
    cancellation: Option<RemoteCancellation>,
    cancellation_reported: bool,
}

impl HttpRangeReader {
    pub fn open(url: &str, config: HttpRangeConfig) -> Result<Self, RemoteError> {
        validate_config(&config)?;
        let url = Url::parse(url)?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(RemoteError::UnsupportedScheme(url.scheme().to_owned()));
        }
        let redirect_count = Arc::new(AtomicU64::new(0));
        let policy_count = Arc::clone(&redirect_count);
        let redirect_policy = Policy::custom(move |attempt| {
            if attempt.previous().len() > 10 {
                return attempt.error(io::Error::other("remote redirect limit exceeded"));
            }
            let next = attempt.url();
            if !matches!(next.scheme(), "http" | "https") {
                return attempt.error(io::Error::other(
                    "remote redirect used an unsupported scheme",
                ));
            }
            if attempt
                .previous()
                .last()
                .is_some_and(|previous| previous.scheme() == "https" && next.scheme() == "http")
            {
                return attempt.error(io::Error::other(
                    "remote redirect attempted a secure transport downgrade",
                ));
            }
            policy_count.fetch_add(1, Ordering::Relaxed);
            attempt.follow()
        });
        let client = Client::builder()
            .timeout(config.request_timeout)
            .redirect(redirect_policy)
            .build()?;
        let (response, probe_requests, probe_retries) =
            send_probe_with_retry(&client, &url, &config, &redirect_count)?;
        if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(RemoteError::ObjectChangedOrRangeUnsupported);
        }
        let (response_start, response_end, length) = parse_content_range(response.headers())?;
        if response_start != 0 || response_end != 0 || length == 0 {
            return Err(RemoteError::UnexpectedContentRange {
                requested_start: 0,
                requested_end: 0,
                response_start,
                response_end,
                response_length: length,
            });
        }
        let validator = Validator {
            etag: response.headers().get(ETAG).cloned(),
            last_modified: response.headers().get(LAST_MODIFIED).cloned(),
        };
        let initial_response_bytes = read_bounded_response(response, 1)?.len() as u64;
        let block_capacity = config
            .maximum_cache_bytes
            .get()
            .checked_div(config.chunk_size.get())
            .unwrap_or_default()
            .max(1);
        Ok(Self {
            client,
            url,
            length,
            position: 0,
            config,
            validator,
            cache_directory: tempfile::tempdir()?,
            blocks: LruCache::new(NonZeroUsize::new(block_capacity).unwrap()),
            current_block: None,
            stats: RemoteIoStats {
                requests: probe_requests,
                response_bytes: initial_response_bytes,
                retries: probe_retries,
                ..RemoteIoStats::default()
            },
            redirect_count,
            cancellation: None,
            cancellation_reported: false,
        })
    }

    pub const fn len(&self) -> u64 {
        self.length
    }

    pub const fn is_empty(&self) -> bool {
        self.length == 0
    }

    pub const fn stats(&self) -> RemoteIoStats {
        self.stats
    }

    pub fn redacted_url(&self) -> String {
        redact_url(&self.url)
    }

    pub fn set_cancellation(&mut self, cancellation: Option<RemoteCancellation>) {
        self.cancellation = cancellation;
        self.cancellation_reported = false;
    }

    fn check_cancellation(&mut self) -> Result<(), RemoteError> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(|keep_going| !keep_going())
        {
            if !self.cancellation_reported {
                self.stats.cancellations = self.stats.cancellations.saturating_add(1);
                self.cancellation_reported = true;
            }
            return Err(RemoteError::Cancelled);
        }
        Ok(())
    }

    fn load_block(&mut self, block_index: u64, requested_bytes: usize) -> io::Result<()> {
        if let Some(block) = self.blocks.get(&block_index) {
            self.current_block = Some((block_index, fs::read(&block.path)?));
            self.stats.cache_hits = self.stats.cache_hits.saturating_add(1);
            return Ok(());
        }
        self.stats.cache_misses = self.stats.cache_misses.saturating_add(1);
        self.fetch_blocks(block_index, requested_bytes)
            .map_err(io::Error::other)?;
        let block = self
            .blocks
            .get(&block_index)
            .ok_or_else(|| io::Error::other("range response did not contain requested block"))?;
        self.current_block = Some((block_index, fs::read(&block.path)?));
        Ok(())
    }

    fn fetch_blocks(
        &mut self,
        first_block: u64,
        requested_bytes: usize,
    ) -> Result<(), RemoteError> {
        let chunk_size = self.config.chunk_size.get() as u64;
        let maximum_chunks = self.config.maximum_coalesced_chunks.get();
        let minimum_chunks = self
            .config
            .minimum_coalesced_chunks
            .get()
            .min(maximum_chunks);
        let desired_chunks = requested_bytes
            .div_ceil(self.config.chunk_size.get())
            .max(minimum_chunks)
            .min(maximum_chunks) as u64;
        let start = first_block.saturating_mul(chunk_size);
        let end = start
            .saturating_add(desired_chunks.saturating_mul(chunk_size))
            .min(self.length)
            .saturating_sub(1);
        let response = self.send_range_with_retry(start, end)?;
        if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(RemoteError::ObjectChangedOrRangeUnsupported);
        }
        validate_response_validator(&self.validator, response.headers())?;
        let (response_start, response_end, response_length) =
            parse_content_range(response.headers())?;
        if response_start != start || response_end != end || response_length != self.length {
            return Err(RemoteError::UnexpectedContentRange {
                requested_start: start,
                requested_end: end,
                response_start,
                response_end,
                response_length,
            });
        }
        let expected_bytes = usize::try_from(end.saturating_sub(start).saturating_add(1))
            .map_err(|_| RemoteError::ResponseTooLarge(u64::MAX))?;
        let bytes = read_bounded_response(response, expected_bytes)?;
        self.stats.response_bytes = self.stats.response_bytes.saturating_add(bytes.len() as u64);
        for (offset, data) in bytes.chunks(self.config.chunk_size.get()).enumerate() {
            let block_index = first_block.saturating_add(offset as u64);
            if self.blocks.get(&block_index).is_some() {
                continue;
            }
            let path = self
                .cache_directory
                .path()
                .join(format!("{block_index:016x}.range"));
            fs::write(&path, data)?;
            if let Some((_, evicted)) = self.blocks.push(block_index, CachedBlock { path }) {
                let _ = fs::remove_file(evicted.path);
                self.stats.evictions = self.stats.evictions.saturating_add(1);
            }
        }
        Ok(())
    }

    fn send_range_with_retry(
        &mut self,
        start: u64,
        end: u64,
    ) -> Result<reqwest::blocking::Response, RemoteError> {
        for attempt in 0..=self.config.maximum_retries {
            self.check_cancellation()?;
            let mut request = self.config.authentication.apply(
                self.client
                    .get(self.url.clone())
                    .header(RANGE, format!("bytes={start}-{end}")),
            );
            if let Some(if_range) = self.validator.if_range() {
                request = request.header(IF_RANGE, if_range);
            }
            self.stats.requests = self.stats.requests.saturating_add(1);
            let redirects_before = self.redirect_count.load(Ordering::Relaxed);
            let response = request.send();
            let redirects = self
                .redirect_count
                .load(Ordering::Relaxed)
                .saturating_sub(redirects_before);
            self.stats.requests = self.stats.requests.saturating_add(redirects);
            self.check_cancellation()?;
            match response {
                Ok(response)
                    if is_retryable_status(response.status())
                        && attempt < self.config.maximum_retries =>
                {
                    self.stats.retries = self.stats.retries.saturating_add(1);
                    thread::sleep(retry_delay(attempt));
                }
                Ok(response) => return Ok(response.error_for_status()?),
                Err(error)
                    if (error.is_timeout() || error.is_connect())
                        && attempt < self.config.maximum_retries =>
                {
                    self.stats.retries = self.stats.retries.saturating_add(1);
                    thread::sleep(retry_delay(attempt));
                }
                Err(error) => return Err(error.into()),
            }
        }
        unreachable!("retry loop always returns on its final attempt")
    }
}

fn send_probe_with_retry(
    client: &Client,
    url: &Url,
    config: &HttpRangeConfig,
    redirect_count: &AtomicU64,
) -> Result<(reqwest::blocking::Response, u64, u64), RemoteError> {
    let mut requests = 0_u64;
    let mut retries = 0_u64;
    for attempt in 0..=config.maximum_retries {
        let redirects_before = redirect_count.load(Ordering::Relaxed);
        requests = requests.saturating_add(1);
        let response = config
            .authentication
            .apply(client.get(url.clone()).header(RANGE, "bytes=0-0"))
            .send();
        requests = requests.saturating_add(
            redirect_count
                .load(Ordering::Relaxed)
                .saturating_sub(redirects_before),
        );
        match response {
            Ok(response)
                if is_retryable_status(response.status()) && attempt < config.maximum_retries =>
            {
                retries = retries.saturating_add(1);
                thread::sleep(retry_delay(attempt));
            }
            Ok(response) => return Ok((response.error_for_status()?, requests, retries)),
            Err(error)
                if (error.is_timeout() || error.is_connect())
                    && attempt < config.maximum_retries =>
            {
                retries = retries.saturating_add(1);
                thread::sleep(retry_delay(attempt));
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!("probe retry loop always returns on its final attempt")
}

fn validate_config(config: &HttpRangeConfig) -> Result<(), RemoteError> {
    if config.maximum_cache_bytes < config.chunk_size {
        return Err(RemoteError::InvalidConfig(
            "maximum cache bytes must be at least one range chunk",
        ));
    }
    if config.minimum_coalesced_chunks > config.maximum_coalesced_chunks {
        return Err(RemoteError::InvalidConfig(
            "minimum coalesced chunks cannot exceed the maximum",
        ));
    }
    if config.request_timeout.is_zero() {
        return Err(RemoteError::InvalidConfig(
            "request timeout must be greater than zero",
        ));
    }
    Ok(())
}

fn parse_content_range(
    headers: &reqwest::header::HeaderMap,
) -> Result<(u64, u64, u64), RemoteError> {
    let value = headers
        .get(CONTENT_RANGE)
        .ok_or(RemoteError::MissingContentRange)?
        .to_str()?;
    let value = value
        .strip_prefix("bytes ")
        .ok_or(RemoteError::InvalidContentRange)?;
    let (range, length) = value
        .split_once('/')
        .ok_or(RemoteError::InvalidContentRange)?;
    let (start, end) = range
        .split_once('-')
        .ok_or(RemoteError::InvalidContentRange)?;
    let start = start
        .parse::<u64>()
        .map_err(|_| RemoteError::InvalidContentRange)?;
    let end = end
        .parse::<u64>()
        .map_err(|_| RemoteError::InvalidContentRange)?;
    let length = length
        .parse::<u64>()
        .map_err(|_| RemoteError::InvalidContentRange)?;
    if end < start || end >= length {
        return Err(RemoteError::InvalidContentRange);
    }
    Ok((start, end, length))
}

fn read_bounded_response(
    mut response: reqwest::blocking::Response,
    expected_bytes: usize,
) -> Result<Vec<u8>, RemoteError> {
    if response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length != expected_bytes as u64)
    {
        return Err(RemoteError::UnexpectedResponseLength {
            expected: expected_bytes as u64,
            actual: response.content_length(),
        });
    }
    let limit = expected_bytes.saturating_add(1) as u64;
    let mut bytes = Vec::with_capacity(expected_bytes.min(1024 * 1024));
    response.by_ref().take(limit).read_to_end(&mut bytes)?;
    if bytes.len() != expected_bytes {
        return Err(RemoteError::UnexpectedResponseLength {
            expected: expected_bytes as u64,
            actual: Some(bytes.len() as u64),
        });
    }
    Ok(bytes)
}

pub fn read_remote_object_bounded(
    url: &str,
    config: HttpRangeConfig,
    maximum_bytes: usize,
) -> Result<(Vec<u8>, RemoteIoStats), RemoteError> {
    let mut reader = HttpRangeReader::open(url, config)?;
    let length =
        usize::try_from(reader.len()).map_err(|_| RemoteError::ResponseTooLarge(u64::MAX))?;
    if length > maximum_bytes {
        return Err(RemoteError::ResponseTooLarge(reader.len()));
    }
    let mut bytes = Vec::with_capacity(length);
    reader.read_to_end(&mut bytes)?;
    let stats = reader.stats();
    Ok((bytes, stats))
}

fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn retry_delay(attempt: usize) -> Duration {
    Duration::from_millis(25_u64.saturating_mul(1_u64 << attempt.min(5)))
}

impl Read for HttpRangeReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.check_cancellation().map_err(io::Error::other)?;
        if buffer.is_empty() || self.position >= self.length {
            return Ok(0);
        }
        let block_size = self.config.chunk_size.get() as u64;
        let block_index = self.position / block_size;
        if self
            .current_block
            .as_ref()
            .is_none_or(|(current, _)| *current != block_index)
        {
            self.load_block(block_index, buffer.len())?;
        }
        let (_, block) = self.current_block.as_ref().expect("block was loaded");
        let offset = usize::try_from(self.position % block_size).map_err(io::Error::other)?;
        let available = block.len().saturating_sub(offset);
        let remaining = usize::try_from(self.length - self.position).unwrap_or(usize::MAX);
        let count = buffer.len().min(available).min(remaining);
        buffer[..count].copy_from_slice(&block[offset..offset + count]);
        self.position = self.position.saturating_add(count as u64);
        Ok(count)
    }
}

impl Seek for HttpRangeReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let next = match position {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::End(offset) => i128::from(self.length) + i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        if !(0..=i128::from(self.length)).contains(&next) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek outside remote object",
            ));
        }
        self.position = next as u64;
        Ok(self.position)
    }
}

fn validate_response_validator(
    expected: &Validator,
    headers: &reqwest::header::HeaderMap,
) -> Result<(), RemoteError> {
    if let Some(etag) = &expected.etag {
        if headers.get(ETAG) != Some(etag) {
            return Err(RemoteError::ObjectChangedOrRangeUnsupported);
        }
    } else if let Some(last_modified) = &expected.last_modified
        && headers.get(LAST_MODIFIED) != Some(last_modified)
    {
        return Err(RemoteError::ObjectChangedOrRangeUnsupported);
    }
    Ok(())
}

pub fn redact_url(url: &Url) -> String {
    let mut redacted = url.clone();
    let _ = redacted.set_username(if url.username().is_empty() {
        ""
    } else {
        "[redacted]"
    });
    let _ = redacted.set_password(url.password().map(|_| "[redacted]"));
    if redacted.query().is_some() {
        redacted.set_query(Some("[redacted]"));
    }
    redacted.to_string()
}

pub fn associated_remote_url(url: &str, suffix: &str) -> Result<String, RemoteError> {
    let mut associated = Url::parse(url)?;
    if !matches!(associated.scheme(), "http" | "https") {
        return Err(RemoteError::UnsupportedScheme(
            associated.scheme().to_owned(),
        ));
    }
    let path = format!("{}{suffix}", associated.path());
    associated.set_path(&path);
    Ok(associated.into())
}

pub fn remote_url_with_extension(url: &str, extension: &str) -> Result<String, RemoteError> {
    let mut associated = Url::parse(url)?;
    if !matches!(associated.scheme(), "http" | "https") {
        return Err(RemoteError::UnsupportedScheme(
            associated.scheme().to_owned(),
        ));
    }
    let path = associated.path();
    let directory_end = path.rfind('/').map_or(0, |index| index + 1);
    let filename = &path[directory_end..];
    let stem = filename.rsplit_once('.').map_or(filename, |(stem, _)| stem);
    associated.set_path(&format!("{}{stem}.{extension}", &path[..directory_end]));
    Ok(associated.into())
}

#[derive(Debug, Error)]
pub enum RemoteError {
    #[error(transparent)]
    Url(#[from] url::ParseError),
    #[error("{0}")]
    Http(#[source] RemoteHttpError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Header(#[from] reqwest::header::ToStrError),
    #[error("remote server did not provide Content-Range")]
    MissingContentRange,
    #[error("remote server returned an invalid Content-Range")]
    InvalidContentRange,
    #[error("unsupported remote URL scheme {0}")]
    UnsupportedScheme(String),
    #[error("remote object changed during the session or stopped honoring byte ranges")]
    ObjectChangedOrRangeUnsupported,
    #[error("invalid remote range configuration: {0}")]
    InvalidConfig(&'static str),
    #[error(
        "remote server returned bytes {response_start}-{response_end}/{response_length} for requested range {requested_start}-{requested_end}"
    )]
    UnexpectedContentRange {
        requested_start: u64,
        requested_end: u64,
        response_start: u64,
        response_end: u64,
        response_length: u64,
    },
    #[error(
        "remote response length did not match the requested range (expected {expected} bytes, received {actual:?})"
    )]
    UnexpectedResponseLength { expected: u64, actual: Option<u64> },
    #[error("remote object exceeds the bounded download limit ({0} bytes)")]
    ResponseTooLarge(u64),
    #[error("remote request was cancelled because the view changed")]
    Cancelled,
}

impl From<reqwest::Error> for RemoteError {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error.into())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
        thread,
        time::Duration,
    };

    use tiny_http::{Header, Response, Server, StatusCode};

    use super::*;

    struct RangeFixture {
        url: String,
        stop: Arc<AtomicBool>,
        requests: Arc<AtomicU64>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl RangeFixture {
        fn start(data: Vec<u8>) -> Option<Self> {
            Self::start_with_conditions(data, Duration::ZERO, false, false, false)
        }

        fn start_with_conditions(
            data: Vec<u8>,
            latency: Duration,
            fail_first_probe: bool,
            fail_first_data_request: bool,
            change_validator_after_probe: bool,
        ) -> Option<Self> {
            let server = Server::http("127.0.0.1:0").ok()?;
            let address = server.server_addr().to_ip()?;
            let stop = Arc::new(AtomicBool::new(false));
            let requests = Arc::new(AtomicU64::new(0));
            let worker_stop = Arc::clone(&stop);
            let worker_requests = Arc::clone(&requests);
            let worker = thread::spawn(move || {
                let mut rejected_data_request = false;
                while !worker_stop.load(Ordering::Relaxed) {
                    let Some(request) = server.recv_timeout(Duration::from_millis(20)).unwrap()
                    else {
                        continue;
                    };
                    let request_number = worker_requests.fetch_add(1, Ordering::Relaxed) + 1;
                    if !latency.is_zero() {
                        thread::sleep(latency);
                    }
                    if fail_first_probe && request_number == 1 {
                        request.respond(Response::empty(StatusCode(503))).unwrap();
                        continue;
                    }
                    if fail_first_data_request && request_number > 1 && !rejected_data_request {
                        rejected_data_request = true;
                        request.respond(Response::empty(StatusCode(503))).unwrap();
                        continue;
                    }
                    let range = request
                        .headers()
                        .iter()
                        .find(|header| header.field.equiv("Range"))
                        .and_then(|header| parse_test_range(header.value.as_str(), data.len()))
                        .unwrap();
                    let body = data[range.0..=range.1].to_vec();
                    let response = Response::from_data(body)
                        .with_status_code(StatusCode(206))
                        .with_header(test_header(
                            "Content-Range",
                            format!("bytes {}-{}/{}", range.0, range.1, data.len()),
                        ))
                        .with_header(test_header(
                            "ETag",
                            if change_validator_after_probe && request_number > 1 {
                                "\"fixture-v2\""
                            } else {
                                "\"fixture-v1\""
                            },
                        ));
                    request.respond(response).unwrap();
                }
            });
            Some(Self {
                url: format!("http://{address}/fixture.bin"),
                stop,
                requests,
                worker: Some(worker),
            })
        }
    }

    impl Drop for RangeFixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(worker) = self.worker.take() {
                worker.join().unwrap();
            }
        }
    }

    #[test]
    fn authentication_debug_output_is_redacted() {
        let authentication = RemoteAuthentication::Bearer("top-secret".to_owned());
        let debug = format!("{authentication:?}");
        assert!(!debug.contains("top-secret"));
    }

    #[test]
    fn urls_hide_user_info_and_query_credentials() {
        let url = Url::parse("https://user:pass@example.org/data.bam?token=secret").unwrap();
        let redacted = redact_url(&url);
        assert!(!redacted.contains("user"));
        assert!(!redacted.contains("pass"));
        assert!(!redacted.contains("secret"));
    }

    #[test]
    fn associated_urls_preserve_signed_queries_and_change_only_the_path() {
        let source = "https://example.org/data/sample.bam?token=temporary#fragment";
        assert_eq!(
            associated_remote_url(source, ".bai").unwrap(),
            "https://example.org/data/sample.bam.bai?token=temporary#fragment"
        );
        assert_eq!(
            remote_url_with_extension(source, "csi").unwrap(),
            "https://example.org/data/sample.csi?token=temporary#fragment"
        );
    }

    #[test]
    fn invalid_range_configuration_is_rejected_before_network_io() {
        let too_small = HttpRangeConfig {
            chunk_size: NonZeroUsize::new(1024).unwrap(),
            maximum_cache_bytes: NonZeroUsize::new(512).unwrap(),
            ..HttpRangeConfig::default()
        };
        assert!(matches!(
            HttpRangeReader::open("https://example.invalid/data", too_small),
            Err(RemoteError::InvalidConfig(_))
        ));

        let reversed_coalescing = HttpRangeConfig {
            minimum_coalesced_chunks: NonZeroUsize::new(4).unwrap(),
            maximum_coalesced_chunks: NonZeroUsize::new(2).unwrap(),
            ..HttpRangeConfig::default()
        };
        assert!(matches!(
            HttpRangeReader::open("https://example.invalid/data", reversed_coalescing),
            Err(RemoteError::InvalidConfig(_))
        ));
    }

    #[test]
    fn content_range_parser_requires_exact_bounded_geometry() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(CONTENT_RANGE, HeaderValue::from_static("bytes 10-19/100"));
        assert_eq!(parse_content_range(&headers).unwrap(), (10, 19, 100));
        headers.insert(CONTENT_RANGE, HeaderValue::from_static("bytes 20-10/100"));
        assert!(matches!(
            parse_content_range(&headers),
            Err(RemoteError::InvalidContentRange)
        ));
        headers.insert(CONTENT_RANGE, HeaderValue::from_static("bytes 10-100/100"));
        assert!(matches!(
            parse_content_range(&headers),
            Err(RemoteError::InvalidContentRange)
        ));
    }

    #[test]
    fn http_errors_never_echo_url_credentials() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let worker = thread::spawn(move || {
            let request = server.recv().unwrap();
            request.respond(Response::empty(StatusCode(404))).unwrap();
        });
        let Err(error) = HttpRangeReader::open(
            &format!("http://private-user:private-pass@{address}/data?token=private-token"),
            HttpRangeConfig::default(),
        ) else {
            panic!("the 404 response unexpectedly opened as a range source");
        };
        let message = error.to_string();
        assert!(message.contains("404"));
        assert!(!message.contains("private-user"));
        assert!(!message.contains("private-pass"));
        assert!(!message.contains("private-token"));
        worker.join().unwrap();
    }

    #[test]
    fn same_origin_redirects_preserve_authentication_and_are_counted() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let server_requests = Arc::new(AtomicU64::new(0));
        let worker_requests = Arc::clone(&server_requests);
        let authenticated = Arc::new(AtomicBool::new(true));
        let worker_authenticated = Arc::clone(&authenticated);
        let worker = thread::spawn(move || {
            let data = b"redirected-range-object";
            while !worker_stop.load(Ordering::Relaxed) {
                let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(20)) else {
                    continue;
                };
                worker_requests.fetch_add(1, Ordering::Relaxed);
                if !request.headers().iter().any(|header| {
                    header.field.equiv("Authorization")
                        && header.value.as_str() == "Bearer redirect-secret"
                }) {
                    worker_authenticated.store(false, Ordering::Relaxed);
                }
                if request.url().starts_with("/redirect") {
                    request
                        .respond(
                            Response::empty(StatusCode(307))
                                .with_header(test_header("Location", "/object?token=preserved")),
                        )
                        .unwrap();
                    continue;
                }
                let range = request
                    .headers()
                    .iter()
                    .find(|header| header.field.equiv("Range"))
                    .and_then(|header| parse_test_range(header.value.as_str(), data.len()))
                    .unwrap();
                request
                    .respond(
                        Response::from_data(data[range.0..=range.1].to_vec())
                            .with_status_code(StatusCode(206))
                            .with_header(test_header(
                                "Content-Range",
                                format!("bytes {}-{}/{}", range.0, range.1, data.len()),
                            ))
                            .with_header(test_header("ETag", "\"redirect-fixture\"")),
                    )
                    .unwrap();
            }
        });
        let mut reader = HttpRangeReader::open(
            &format!("http://{address}/redirect?token=private"),
            HttpRangeConfig {
                chunk_size: NonZeroUsize::new(8).unwrap(),
                maximum_cache_bytes: NonZeroUsize::new(32).unwrap(),
                authentication: RemoteAuthentication::Bearer("redirect-secret".to_owned()),
                ..HttpRangeConfig::default()
            },
        )
        .unwrap();
        let mut bytes = [0_u8; 4];
        reader.read_exact(&mut bytes).unwrap();
        stop.store(true, Ordering::Relaxed);
        worker.join().unwrap();
        assert_eq!(&bytes, b"redi");
        assert!(authenticated.load(Ordering::Relaxed));
        assert_eq!(
            reader.stats().requests,
            server_requests.load(Ordering::Relaxed)
        );
        assert_eq!(reader.stats().requests, 4);
    }

    #[test]
    fn range_reads_stop_at_a_cancelled_generation_boundary() {
        let Some(fixture) = RangeFixture::start((0_u8..=255).cycle().take(4_096).collect()) else {
            return;
        };
        let mut reader = HttpRangeReader::open(&fixture.url, HttpRangeConfig::default()).unwrap();
        let cancellation: RemoteCancellation = Arc::new(|| false);
        reader.set_cancellation(Some(cancellation));
        let mut byte = [0_u8; 1];
        let error = reader.read_exact(&mut byte).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(error.to_string().contains("cancelled"));
        assert_eq!(reader.stats().cancellations, 1);
        assert_eq!(reader.stats().requests, 1);
    }

    #[test]
    fn in_flight_range_result_is_discarded_when_its_generation_is_cancelled() {
        let Some(fixture) = RangeFixture::start_with_conditions(
            (0_u8..=255).cycle().take(4_096).collect(),
            Duration::from_millis(40),
            false,
            false,
            false,
        ) else {
            return;
        };
        let mut reader = HttpRangeReader::open(
            &fixture.url,
            HttpRangeConfig {
                chunk_size: NonZeroUsize::new(256).unwrap(),
                maximum_cache_bytes: NonZeroUsize::new(1_024).unwrap(),
                request_timeout: Duration::from_secs(1),
                ..HttpRangeConfig::default()
            },
        )
        .unwrap();
        let keep_going = Arc::new(AtomicBool::new(true));
        let cancellation_flag = Arc::clone(&keep_going);
        let cancellation: RemoteCancellation =
            Arc::new(move || cancellation_flag.load(Ordering::Relaxed));
        reader.set_cancellation(Some(cancellation));
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            keep_going.store(false, Ordering::Relaxed);
        });
        let error = reader.read_exact(&mut [0; 8]).unwrap_err();
        canceller.join().unwrap();
        assert!(error.to_string().contains("cancelled"));
        assert_eq!(reader.stats().cancellations, 1);
        assert_eq!(reader.stats().requests, 2);
    }

    #[test]
    fn seek_rejects_positions_outside_object() {
        let next = i128::from(10_u64) + i128::from(-11_i64);
        assert!(!(0..=i128::from(10_u64)).contains(&next));
    }

    #[test]
    fn range_reader_coalesces_and_reuses_disk_cached_blocks() {
        let Some(fixture) = RangeFixture::start((0_u8..=255).cycle().take(4_096).collect()) else {
            return;
        };
        let mut reader = HttpRangeReader::open(
            &fixture.url,
            HttpRangeConfig {
                chunk_size: NonZeroUsize::new(256).unwrap(),
                maximum_cache_bytes: NonZeroUsize::new(1_024).unwrap(),
                maximum_coalesced_chunks: NonZeroUsize::new(4).unwrap(),
                authentication: RemoteAuthentication::None,
                ..HttpRangeConfig::default()
            },
        )
        .unwrap();
        let mut buffer = vec![0; 600];
        reader.read_exact(&mut buffer).unwrap();
        assert_eq!(buffer[0], 0);
        assert_eq!(buffer[599], 87);
        let requests_after_first_read = fixture.requests.load(Ordering::Relaxed);
        reader.seek(SeekFrom::Start(0)).unwrap();
        reader.read_exact(&mut buffer[..128]).unwrap();
        assert_eq!(
            fixture.requests.load(Ordering::Relaxed),
            requests_after_first_read
        );
        assert!(reader.stats().cache_hits > 0);
        assert_eq!(reader.stats().response_bytes, 769);
    }

    #[test]
    fn disk_range_cache_evicts_blocks_at_its_configured_bound() {
        let Some(fixture) = RangeFixture::start((0_u8..=255).cycle().take(4_096).collect()) else {
            return;
        };
        let mut reader = HttpRangeReader::open(
            &fixture.url,
            HttpRangeConfig {
                chunk_size: NonZeroUsize::new(256).unwrap(),
                maximum_cache_bytes: NonZeroUsize::new(512).unwrap(),
                maximum_coalesced_chunks: NonZeroUsize::MIN,
                ..HttpRangeConfig::default()
            },
        )
        .unwrap();
        reader.read_exact(&mut [0; 768]).unwrap();
        assert_eq!(reader.stats().cache_misses, 3);
        assert_eq!(reader.stats().evictions, 1);
        let requests_before_revisit = reader.stats().requests;
        reader.seek(SeekFrom::Start(0)).unwrap();
        reader.read_exact(&mut [0; 1]).unwrap();
        assert_eq!(reader.stats().requests, requests_before_revisit + 1);
        assert_eq!(reader.stats().evictions, 2);
    }

    #[test]
    fn simulated_wan_latency_loss_and_byte_budget_are_observable() {
        let Some(fixture) = RangeFixture::start_with_conditions(
            (0_u8..=255).cycle().take(4_096).collect(),
            Duration::from_millis(5),
            false,
            true,
            false,
        ) else {
            return;
        };
        let started = std::time::Instant::now();
        let mut reader = HttpRangeReader::open(
            &fixture.url,
            HttpRangeConfig {
                chunk_size: NonZeroUsize::new(256).unwrap(),
                maximum_cache_bytes: NonZeroUsize::new(1_024).unwrap(),
                maximum_coalesced_chunks: NonZeroUsize::new(4).unwrap(),
                request_timeout: Duration::from_secs(1),
                maximum_retries: 1,
                ..HttpRangeConfig::default()
            },
        )
        .unwrap();
        let mut buffer = vec![0; 600];
        reader.read_exact(&mut buffer).unwrap();
        let stats = reader.stats();
        assert_eq!(stats.requests, 3);
        assert_eq!(stats.retries, 1);
        assert_eq!(stats.response_bytes, 769);
        assert!(started.elapsed() >= Duration::from_millis(15));
        assert_eq!(buffer[599], 87);
    }

    #[test]
    fn initial_range_probe_retries_transient_failures() {
        let Some(fixture) = RangeFixture::start_with_conditions(
            (0_u8..=255).cycle().take(4_096).collect(),
            Duration::ZERO,
            true,
            false,
            false,
        ) else {
            return;
        };
        let reader = HttpRangeReader::open(
            &fixture.url,
            HttpRangeConfig {
                maximum_retries: 1,
                ..HttpRangeConfig::default()
            },
        )
        .unwrap();
        assert_eq!(reader.stats().requests, 2);
        assert_eq!(reader.stats().retries, 1);
    }

    #[test]
    fn changed_etag_invalidates_the_remote_object() {
        let Some(fixture) = RangeFixture::start_with_conditions(
            (0_u8..=255).cycle().take(4_096).collect(),
            Duration::ZERO,
            false,
            false,
            true,
        ) else {
            return;
        };
        let mut reader = HttpRangeReader::open(
            &fixture.url,
            HttpRangeConfig {
                chunk_size: NonZeroUsize::new(256).unwrap(),
                maximum_cache_bytes: NonZeroUsize::new(1_024).unwrap(),
                ..HttpRangeConfig::default()
            },
        )
        .unwrap();
        let error = reader.read_exact(&mut [0; 8]).unwrap_err();
        assert!(error.to_string().contains("remote object changed"));

        drop(reader);
        let mut reopened = HttpRangeReader::open(
            &fixture.url,
            HttpRangeConfig {
                chunk_size: NonZeroUsize::new(256).unwrap(),
                maximum_cache_bytes: NonZeroUsize::new(1_024).unwrap(),
                ..HttpRangeConfig::default()
            },
        )
        .unwrap();
        let mut bytes = [0; 8];
        reopened.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, [0, 1, 2, 3, 4, 5, 6, 7]);
        assert!(fixture.requests.load(Ordering::Relaxed) >= 4);
    }

    #[test]
    fn short_range_body_is_rejected_instead_of_becoming_partial_data() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let data = (0_u8..64).collect::<Vec<_>>();
            let mut request_count = 0_u64;
            while !worker_stop.load(Ordering::Relaxed) {
                let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(20)) else {
                    continue;
                };
                request_count = request_count.saturating_add(1);
                let (start, end) = request
                    .headers()
                    .iter()
                    .find(|header| header.field.equiv("Range"))
                    .and_then(|header| parse_test_range(header.value.as_str(), data.len()))
                    .unwrap();
                let body_end = if request_count == 1 {
                    end.saturating_add(1)
                } else {
                    end
                };
                request
                    .respond(
                        Response::from_data(data[start..body_end].to_vec())
                            .with_status_code(StatusCode(206))
                            .with_header(test_header(
                                "Content-Range",
                                format!("bytes {start}-{end}/{}", data.len()),
                            ))
                            .with_header(test_header("ETag", "\"short-body-fixture\"")),
                    )
                    .unwrap();
            }
        });
        let mut reader = HttpRangeReader::open(
            &format!("http://{address}/short.bin"),
            HttpRangeConfig {
                chunk_size: NonZeroUsize::new(8).unwrap(),
                maximum_cache_bytes: NonZeroUsize::new(16).unwrap(),
                maximum_coalesced_chunks: NonZeroUsize::MIN,
                ..HttpRangeConfig::default()
            },
        )
        .unwrap();
        let error = reader.read_exact(&mut [0; 8]).unwrap_err();
        stop.store(true, Ordering::Relaxed);
        worker.join().unwrap();
        assert!(
            error
                .to_string()
                .contains("response length did not match the requested range")
        );
        assert!(error.to_string().contains("expected 8 bytes"));
        assert!(error.to_string().contains("received Some(7)"));
    }

    fn parse_test_range(value: &str, length: usize) -> Option<(usize, usize)> {
        let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
        let start = start.parse().ok()?;
        let end = end.parse::<usize>().ok()?.min(length.saturating_sub(1));
        (start <= end).then_some((start, end))
    }

    fn test_header(name: &str, value: impl AsRef<str>) -> Header {
        Header::from_bytes(name.as_bytes(), value.as_ref().as_bytes()).unwrap()
    }
}
