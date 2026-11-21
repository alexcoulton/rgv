use std::{
    collections::BTreeMap,
    fmt, fs,
    hash::{BuildHasher, Hasher},
    io::{self, Read, Seek, SeekFrom},
    num::NonZeroUsize,
    ops::Range,
    path::PathBuf,
    sync::{Arc, OnceLock},
    thread,
    time::Duration,
};

use lru::LruCache;
use reqwest::{
    blocking::{Client, RequestBuilder},
    header::{
        AUTHORIZATION, CONTENT_LENGTH, CONTENT_RANGE, ETAG, HeaderName, HeaderValue, IF_RANGE,
        LAST_MODIFIED, LOCATION, RANGE, RETRY_AFTER,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteTransferBudget {
    pub maximum_requests: u64,
    pub maximum_response_bytes: u64,
}

#[derive(Clone, Copy, Debug)]
struct ActiveRemoteTransferBudget {
    baseline: RemoteIoStats,
    budget: RemoteTransferBudget,
}

pub type RemoteCancellation = Arc<dyn Fn() -> bool + Send + Sync>;

#[derive(Clone, Debug)]
struct Validator {
    etag: Option<HeaderValue>,
    last_modified: Option<HeaderValue>,
}

/// Validator snapshot captured by the opening range probe.
///
/// Header values remain opaque bytes. They are used only as revision material and are never
/// exposed through diagnostics, which avoids leaking signed object metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteObjectVersion {
    pub(crate) length: u64,
    pub(crate) etag: Option<Vec<u8>>,
    pub(crate) last_modified: Option<Vec<u8>>,
}

impl Validator {
    /// `If-Range` accepts only a strong entity tag or a date (RFC 9110 section 13.1.5). A weak
    /// tag there makes a compliant server ignore the range and answer `200`, which would be
    /// misreported as a changed object; the weak tag is still compared on every response.
    fn if_range(&self) -> Option<HeaderValue> {
        self.etag
            .as_ref()
            .filter(|etag| is_strong_entity_tag(etag))
            .or(self.last_modified.as_ref())
            .cloned()
    }
}

fn is_strong_entity_tag(etag: &HeaderValue) -> bool {
    let bytes = etag.as_bytes();
    bytes.len() >= 2 && bytes.starts_with(b"\"") && bytes.ends_with(b"\"")
}

#[derive(Debug)]
struct CachedBlock {
    path: PathBuf,
}

/// Recently used cache blocks kept in memory, so moving between them (a BGZF reader does it
/// constantly) costs no file read. A subset of the disk tier, never beyond this many bytes.
const MEMORY_BLOCK_BYTES: usize = 4 * 1024 * 1024;
/// Bytes of planned extents one reader keeps in memory.
const PLANNED_EXTENT_MEMORY_BYTES: usize = 16 * 1024 * 1024;
/// Two planned ranges closer than this are fetched as one: the bytes in the gap cost less than
/// another round trip.
const PLAN_COALESCE_GAP_BYTES: u64 = 64 * 1024;
/// A planned range longer than one read-ahead window is fetched in requests that double in
/// size up to this many windows, so a deep region costs few round trips while a query that
/// stops early has not paid for the whole range.
const PLANNED_REQUEST_GROWTH_LIMIT: u64 = 4;
/// Largest BGZF block (`BSIZE` is a 16-bit length minus one).
const BGZF_MAXIMUM_BLOCK_BYTES: u64 = 64 * 1024;

/// Bytes fetched for a read plan, exactly as requested rather than aligned to cache blocks.
struct PlannedExtent {
    data: Vec<u8>,
    last_used: u64,
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
    memory_blocks: LruCache<u64, Arc<[u8]>>,
    current_block: Option<(u64, Arc<[u8]>)>,
    /// Coalesced, sorted byte ranges the caller expects to read (see [`Self::set_read_plan`]).
    plan: Vec<Range<u64>>,
    /// Size of the next planned request; grows while a long range is read through.
    plan_request_bytes: u64,
    /// Planned bytes held in memory, keyed by their first offset; never overlapping.
    extents: BTreeMap<u64, PlannedExtent>,
    extent_bytes: usize,
    extent_clock: u64,
    current_extent: Option<u64>,
    stats: RemoteIoStats,
    cancellation: Option<RemoteCancellation>,
    cancellation_reported: bool,
    transfer_budget: Option<ActiveRemoteTransferBudget>,
}

impl HttpRangeReader {
    pub fn open(url: &str, config: HttpRangeConfig) -> Result<Self, RemoteError> {
        validate_config(&config)?;
        let url = Url::parse(url)?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(RemoteError::UnsupportedScheme(url.scheme().to_owned()));
        }
        let client = shared_client()?;
        let (response, probe_requests, probe_retries) =
            send_probe_with_retry(&client, &url, &config)?;
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
        let initial_response_bytes = read_bounded_response(response, 1, None)?.len() as u64;
        let block_capacity = config
            .maximum_cache_bytes
            .get()
            .checked_div(config.chunk_size.get())
            .unwrap_or_default()
            .max(1);
        let memory_block_capacity = MEMORY_BLOCK_BYTES
            .checked_div(config.chunk_size.get())
            .unwrap_or_default()
            .clamp(1, block_capacity);
        Ok(Self {
            client,
            url,
            length,
            position: 0,
            config,
            validator,
            cache_directory: tempfile::tempdir()?,
            blocks: LruCache::new(NonZeroUsize::new(block_capacity).unwrap()),
            memory_blocks: LruCache::new(NonZeroUsize::new(memory_block_capacity).unwrap()),
            current_block: None,
            plan: Vec::new(),
            plan_request_bytes: 0,
            extents: BTreeMap::new(),
            extent_bytes: 0,
            extent_clock: 0,
            current_extent: None,
            stats: RemoteIoStats {
                requests: probe_requests,
                response_bytes: initial_response_bytes,
                retries: probe_retries,
                ..RemoteIoStats::default()
            },
            cancellation: None,
            cancellation_reported: false,
            transfer_budget: None,
        })
    }

    pub const fn len(&self) -> u64 {
        self.length
    }

    pub const fn is_empty(&self) -> bool {
        self.length == 0
    }

    pub(crate) fn object_version(&self) -> RemoteObjectVersion {
        RemoteObjectVersion {
            length: self.length,
            etag: self
                .validator
                .etag
                .as_ref()
                .map(|value| value.as_bytes().to_vec()),
            last_modified: self
                .validator
                .last_modified
                .as_ref()
                .map(|value| value.as_bytes().to_vec()),
        }
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

    pub fn set_transfer_budget(&mut self, budget: Option<RemoteTransferBudget>) {
        self.transfer_budget = budget.map(|budget| ActiveRemoteTransferBudget {
            baseline: self.stats,
            budget,
        });
    }

    fn check_transfer_budget(&self, expected_response_bytes: u64) -> Result<(), RemoteError> {
        let Some(active) = self.transfer_budget else {
            return Ok(());
        };
        let requests = self.stats.requests.saturating_sub(active.baseline.requests);
        let response_bytes = self
            .stats
            .response_bytes
            .saturating_sub(active.baseline.response_bytes);
        if requests >= active.budget.maximum_requests
            || response_bytes.saturating_add(expected_response_bytes)
                > active.budget.maximum_response_bytes
        {
            return Err(RemoteError::TransferBudgetExceeded);
        }
        Ok(())
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

    /// Declares the byte ranges the coming reads are expected to touch, typically the index
    /// chunks of one region query. A read that misses the cache inside a planned range fetches
    /// from the read position to the end of that range (in bounded requests) instead of a
    /// fixed block-aligned window, so a small region transfers about what the index implies
    /// and a region of many chunks transfers a few coalesced ranges. Ranges closer than
    /// [`PLAN_COALESCE_GAP_BYTES`] are merged. Reads outside every range behave as before.
    pub(crate) fn set_read_plan(&mut self, ranges: impl IntoIterator<Item = Range<u64>>) {
        let mut ranges = ranges
            .into_iter()
            .map(|range| range.start..range.end.min(self.length))
            .filter(|range| range.start < range.end)
            .collect::<Vec<_>>();
        ranges.sort_unstable_by_key(|range| (range.start, range.end));
        let mut plan = Vec::<Range<u64>>::with_capacity(ranges.len());
        for range in ranges {
            match plan.last_mut() {
                Some(last) if range.start <= last.end.saturating_add(PLAN_COALESCE_GAP_BYTES) => {
                    last.end = last.end.max(range.end);
                }
                _ => plan.push(range),
            }
        }
        // A plan that only narrows the previous one (a resumed stream) keeps its request size.
        let continues = !self.plan.is_empty()
            && plan.iter().all(|range| {
                self.plan
                    .iter()
                    .any(|previous| previous.start <= range.start && range.end <= previous.end)
            });
        if !continues {
            self.plan_request_bytes = self.planned_request_bytes().0;
        }
        self.plan = plan;
    }

    /// Plans the compressed byte ranges of BGZF index chunks given as `(start, end)` virtual
    /// offsets. A chunk's last record ends inside the block its end offset names, so that
    /// whole block (of unknown size, at most 64 KiB) is part of the range.
    pub(crate) fn set_bgzf_read_plan(&mut self, chunks: impl IntoIterator<Item = (u64, u64)>) {
        self.set_read_plan(chunks.into_iter().map(|(start, end)| {
            (start >> 16)..(end >> 16).saturating_add(BGZF_MAXIMUM_BLOCK_BYTES)
        }));
    }

    pub(crate) fn clear_read_plan(&mut self) {
        self.plan.clear();
    }

    fn extent_memory_budget(&self) -> usize {
        PLANNED_EXTENT_MEMORY_BYTES.min(self.config.maximum_cache_bytes.get())
    }

    /// First and largest size of a planned request.
    fn planned_request_bytes(&self) -> (u64, u64) {
        let budget = self.extent_memory_budget() as u64;
        let window = (self.config.chunk_size.get() as u64)
            .saturating_mul(self.config.maximum_coalesced_chunks.get() as u64);
        let first = window.min(budget).max(1);
        let largest = window
            .saturating_mul(PLANNED_REQUEST_GROWTH_LIMIT)
            .min(budget / 2)
            .max(first);
        (first, largest)
    }

    /// Start of the cached extent holding `position`, if any.
    fn extent_containing(&self, position: u64) -> Option<u64> {
        self.extents
            .range(..=position)
            .next_back()
            .filter(|(start, extent)| position - **start < extent.data.len() as u64)
            .map(|(start, _)| *start)
    }

    /// Serves a read from a planned extent, fetching it first when the position lies in the
    /// read plan. `None` hands the read to the block cache.
    fn read_planned(&mut self, buffer: &mut [u8]) -> io::Result<Option<usize>> {
        if self.extents.is_empty() && self.plan.is_empty() {
            return Ok(None);
        }
        let position = self.position;
        let start = if let Some(start) = self.extent_containing(position) {
            if self.current_extent != Some(start) {
                self.stats.cache_hits = self.stats.cache_hits.saturating_add(1);
            }
            start
        } else {
            let Some(range_end) = self
                .plan
                .iter()
                .find(|range| range.contains(&position))
                .map(|range| range.end)
            else {
                return Ok(None);
            };
            let block_index = position / self.config.chunk_size.get() as u64;
            if self.blocks.contains(&block_index) {
                // Already paid for by an earlier unplanned read.
                return Ok(None);
            }
            self.stats.cache_misses = self.stats.cache_misses.saturating_add(1);
            self.fetch_planned(position, range_end)
                .map_err(io::Error::other)?;
            position
        };
        self.current_extent = Some(start);
        self.extent_clock = self.extent_clock.saturating_add(1);
        let extent = self
            .extents
            .get_mut(&start)
            .expect("the extent was found or just fetched");
        extent.last_used = self.extent_clock;
        let offset = usize::try_from(position - start).map_err(io::Error::other)?;
        let count = buffer.len().min(extent.data.len() - offset);
        buffer[..count].copy_from_slice(&extent.data[offset..offset + count]);
        self.position = self.position.saturating_add(count as u64);
        Ok(Some(count))
    }

    /// Fetches `[start, range_end)` or its first part, never re-fetching a cached extent and
    /// never asking for more than the active transfer budget still allows.
    fn fetch_planned(&mut self, start: u64, range_end: u64) -> Result<(), RemoteError> {
        let mut end = range_end
            .min(self.length)
            .min(start.saturating_add(self.plan_request_bytes.max(1)));
        if let Some((next, _)) = self.extents.range(start..).next() {
            end = end.min(*next);
        }
        if let Some(active) = self.transfer_budget {
            let remaining = active.budget.maximum_response_bytes.saturating_sub(
                self.stats
                    .response_bytes
                    .saturating_sub(active.baseline.response_bytes),
            );
            if remaining > 0 {
                end = end.min(start.saturating_add(remaining));
            }
        }
        debug_assert!(end > start, "a planned fetch starts inside its range");
        let bytes = self.fetch_range_with_retry(start, end.saturating_sub(1))?;
        self.stats.response_bytes = self.stats.response_bytes.saturating_add(bytes.len() as u64);
        self.plan_request_bytes = self
            .plan_request_bytes
            .saturating_mul(2)
            .min(self.planned_request_bytes().1);
        self.extent_bytes = self.extent_bytes.saturating_add(bytes.len());
        self.extents.insert(
            start,
            PlannedExtent {
                data: bytes,
                last_used: self.extent_clock,
            },
        );
        let budget = self.extent_memory_budget();
        while self.extent_bytes > budget {
            let Some(victim) = self
                .extents
                .iter()
                .filter(|(key, _)| **key != start)
                .min_by_key(|(_, extent)| extent.last_used)
                .map(|(key, _)| *key)
            else {
                break;
            };
            if let Some(evicted) = self.extents.remove(&victim) {
                self.extent_bytes = self.extent_bytes.saturating_sub(evicted.data.len());
                self.stats.evictions = self.stats.evictions.saturating_add(1);
            }
        }
        Ok(())
    }

    fn load_block(&mut self, block_index: u64, requested_bytes: usize) -> io::Result<()> {
        if self.blocks.get(&block_index).is_some() {
            let data = self.cached_block_data(block_index)?;
            self.current_block = Some((block_index, data));
            self.stats.cache_hits = self.stats.cache_hits.saturating_add(1);
            return Ok(());
        }
        self.stats.cache_misses = self.stats.cache_misses.saturating_add(1);
        self.fetch_blocks(block_index, requested_bytes)
            .map_err(io::Error::other)?;
        if self.blocks.get(&block_index).is_none() {
            return Err(io::Error::other(
                "range response did not contain requested block",
            ));
        }
        let data = self.cached_block_data(block_index)?;
        self.current_block = Some((block_index, data));
        Ok(())
    }

    /// Bytes of a block the disk tier holds: from memory when recently used, else read from
    /// its file once and remembered.
    fn cached_block_data(&mut self, block_index: u64) -> io::Result<Arc<[u8]>> {
        if let Some(data) = self.memory_blocks.get(&block_index) {
            return Ok(Arc::clone(data));
        }
        let block = self
            .blocks
            .peek(&block_index)
            .ok_or_else(|| io::Error::other("cache block disappeared"))?;
        let data: Arc<[u8]> = fs::read(&block.path)?.into();
        self.memory_blocks.put(block_index, Arc::clone(&data));
        Ok(data)
    }

    fn fetch_blocks(
        &mut self,
        first_block: u64,
        requested_bytes: usize,
    ) -> Result<(), RemoteError> {
        let chunk_size = self.config.chunk_size.get() as u64;
        // Minimum read-ahead is speculative: cache capacity takes precedence.
        // Otherwise inserting the response can evict its demand block before
        // load_block has had a chance to deliver it.
        let maximum_chunks = self
            .config
            .maximum_coalesced_chunks
            .get()
            .min(self.blocks.cap().get());
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
        let bytes = self.fetch_range_with_retry(start, end)?;
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
            self.memory_blocks.put(block_index, data.into());
            if let Some((evicted_index, evicted)) =
                self.blocks.push(block_index, CachedBlock { path })
            {
                self.memory_blocks.pop(&evicted_index);
                let _ = fs::remove_file(evicted.path);
                self.stats.evictions = self.stats.evictions.saturating_add(1);
            }
        }
        Ok(())
    }

    /// Fetches one byte range, retrying transient failures of the request *and* of the body
    /// transfer: a connection dropped halfway through a body is as transient as a refused one.
    fn fetch_range_with_retry(&mut self, start: u64, end: u64) -> Result<Vec<u8>, RemoteError> {
        let expected_response_bytes = end.saturating_sub(start).saturating_add(1);
        let mut attempt = 0;
        loop {
            self.check_cancellation()?;
            self.check_transfer_budget(expected_response_bytes)?;
            match self.fetch_range_once(start, end) {
                Ok(bytes) => return Ok(bytes),
                Err(RangeAttemptError::Transient { retry_after, .. })
                    if attempt < self.config.maximum_retries =>
                {
                    self.stats.retries = self.stats.retries.saturating_add(1);
                    let delay = retry_delay(attempt, retry_after);
                    let cancellation = self.cancellation.clone();
                    sleep_unless_cancelled(delay, cancellation.as_ref());
                    attempt += 1;
                }
                Err(
                    RangeAttemptError::Transient { error, .. } | RangeAttemptError::Fatal(error),
                ) => return Err(error),
            }
        }
    }

    fn fetch_range_once(&mut self, start: u64, end: u64) -> Result<Vec<u8>, RangeAttemptError> {
        self.stats.requests = self.stats.requests.saturating_add(1);
        let range = format!("bytes={start}-{end}");
        let if_range = self.validator.if_range();
        let (response, redirects) = send_following_redirects(
            &self.client,
            &self.url,
            &self.config,
            &range,
            if_range.as_ref(),
        );
        self.stats.requests = self.stats.requests.saturating_add(redirects);
        self.check_cancellation()
            .map_err(RangeAttemptError::Fatal)?;
        let response = classify_response(response)?;
        if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(RangeAttemptError::Fatal(
                RemoteError::ObjectChangedOrRangeUnsupported,
            ));
        }
        validate_response_validator(&self.validator, response.headers())
            .map_err(RangeAttemptError::Fatal)?;
        let (response_start, response_end, response_length) =
            parse_content_range(response.headers()).map_err(RangeAttemptError::Fatal)?;
        if response_start != start || response_end != end || response_length != self.length {
            return Err(RangeAttemptError::Fatal(
                RemoteError::UnexpectedContentRange {
                    requested_start: start,
                    requested_end: end,
                    response_start,
                    response_end,
                    response_length,
                },
            ));
        }
        let expected_bytes = usize::try_from(end.saturating_sub(start).saturating_add(1))
            .map_err(|_| RangeAttemptError::Fatal(RemoteError::ResponseTooLarge(u64::MAX)))?;
        if declared_length_mismatch(&response, expected_bytes) {
            // The server itself announced the wrong size; repeating the request cannot help.
            return Err(RangeAttemptError::Fatal(
                RemoteError::UnexpectedResponseLength {
                    expected: expected_bytes as u64,
                    actual: response.content_length(),
                },
            ));
        }
        let cancellation = self.cancellation.clone();
        match read_bounded_response(response, expected_bytes, cancellation.as_ref()) {
            Ok(bytes) => Ok(bytes),
            Err(RemoteError::Cancelled) => {
                // Routes through the reader so the cancellation is counted exactly once.
                self.check_cancellation()
                    .map_err(RangeAttemptError::Fatal)?;
                Err(RangeAttemptError::Fatal(RemoteError::Cancelled))
            }
            Err(
                error @ (RemoteError::Io(_)
                | RemoteError::UnexpectedResponseLength {
                    actual: Some(_), ..
                }),
            ) => Err(RangeAttemptError::Transient {
                error,
                retry_after: None,
            }),
            Err(error) => Err(RangeAttemptError::Fatal(error)),
        }
    }
}

/// Outcome of one range attempt that did not produce bytes.
enum RangeAttemptError {
    /// Worth repeating: timeout, connection failure, `408`/`429`/`5xx`, or a body that broke
    /// off mid-transfer.
    Transient {
        error: RemoteError,
        retry_after: Option<Duration>,
    },
    Fatal(RemoteError),
}

enum SendError {
    Transport(reqwest::Error),
    Redirect,
}

impl From<SendError> for RemoteError {
    fn from(error: SendError) -> Self {
        match error {
            SendError::Transport(error) => error.into(),
            SendError::Redirect => Self::Http(RemoteHttpError {
                status: None,
                category: "failed while following a redirect",
            }),
        }
    }
}

/// One connection pool for every remote reader in the process, so opening a second track on the
/// same host reuses established (TLS) connections. Redirects are followed by
/// [`send_following_redirects`] because the policy depends on each reader's opening origin.
fn shared_client() -> Result<Client, RemoteError> {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client.clone());
    }
    let client = Client::builder().redirect(Policy::none()).build()?;
    Ok(CLIENT.get_or_init(|| client).clone())
}

const MAXIMUM_REDIRECTS: u64 = 10;

/// Sends one range request, following redirects under the reader's policy: at most ten hops,
/// `http`/`https` only, never from `https` down to `http`, and never away from the opening
/// origin while a credential is attached (a custom credential header is not something an HTTP
/// client strips on its own). Returns the response and the number of redirects followed.
fn send_following_redirects(
    client: &Client,
    url: &Url,
    config: &HttpRangeConfig,
    range: &str,
    if_range: Option<&HeaderValue>,
) -> (Result<reqwest::blocking::Response, SendError>, u64) {
    let authentication_origin =
        (!matches!(config.authentication, RemoteAuthentication::None)).then(|| url.origin());
    let mut current = url.clone();
    let mut redirects = 0_u64;
    loop {
        let mut request = config.authentication.apply(
            client
                .get(current.clone())
                .timeout(config.request_timeout)
                .header(RANGE, range),
        );
        if let Some(if_range) = if_range {
            request = request.header(IF_RANGE, if_range);
        }
        let response = match request.send() {
            Ok(response) => response,
            Err(error) => return (Err(SendError::Transport(error)), redirects),
        };
        let is_redirect = matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308);
        let Some(location) = response.headers().get(LOCATION).filter(|_| is_redirect) else {
            return (Ok(response), redirects);
        };
        let next = location
            .to_str()
            .ok()
            .and_then(|location| current.join(location).ok());
        let Some(next) = next.filter(|next| {
            redirects < MAXIMUM_REDIRECTS
                && matches!(next.scheme(), "http" | "https")
                && !(current.scheme() == "https" && next.scheme() == "http")
                && authentication_origin
                    .as_ref()
                    .is_none_or(|origin| *origin == next.origin())
        }) else {
            return (Err(SendError::Redirect), redirects);
        };
        redirects += 1;
        current = next;
    }
}

/// Separates responses worth retrying from final ones.
fn classify_response(
    response: Result<reqwest::blocking::Response, SendError>,
) -> Result<reqwest::blocking::Response, RangeAttemptError> {
    match response {
        Ok(response) if is_retryable_status(response.status()) => {
            let retry_after = parse_retry_after(response.headers());
            match response.error_for_status() {
                Ok(response) => Ok(response),
                Err(error) => Err(RangeAttemptError::Transient {
                    error: error.into(),
                    retry_after,
                }),
            }
        }
        Ok(response) => response
            .error_for_status()
            .map_err(|error| RangeAttemptError::Fatal(error.into())),
        Err(SendError::Transport(error)) if error.is_timeout() || error.is_connect() => {
            Err(RangeAttemptError::Transient {
                error: error.into(),
                retry_after: None,
            })
        }
        Err(error) => Err(RangeAttemptError::Fatal(error.into())),
    }
}

fn send_probe_with_retry(
    client: &Client,
    url: &Url,
    config: &HttpRangeConfig,
) -> Result<(reqwest::blocking::Response, u64, u64), RemoteError> {
    let mut requests = 0_u64;
    let mut retries = 0_u64;
    let mut attempt = 0;
    loop {
        requests = requests.saturating_add(1);
        let (response, redirects) =
            send_following_redirects(client, url, config, "bytes=0-0", None);
        requests = requests.saturating_add(redirects);
        match classify_response(response) {
            Ok(response) => return Ok((response, requests, retries)),
            Err(RangeAttemptError::Transient { retry_after, .. })
                if attempt < config.maximum_retries =>
            {
                retries = retries.saturating_add(1);
                sleep_unless_cancelled(retry_delay(attempt, retry_after), None);
                attempt += 1;
            }
            Err(RangeAttemptError::Transient { error, .. } | RangeAttemptError::Fatal(error)) => {
                return Err(error);
            }
        }
    }
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

fn declared_length_mismatch(response: &reqwest::blocking::Response, expected_bytes: usize) -> bool {
    response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length != expected_bytes as u64)
}

/// Body bytes read between two cancellation checks.
const BODY_READ_SLICE_BYTES: usize = 64 * 1024;

fn read_bounded_response(
    mut response: reqwest::blocking::Response,
    expected_bytes: usize,
    cancellation: Option<&RemoteCancellation>,
) -> Result<Vec<u8>, RemoteError> {
    if declared_length_mismatch(&response, expected_bytes) {
        return Err(RemoteError::UnexpectedResponseLength {
            expected: expected_bytes as u64,
            actual: response.content_length(),
        });
    }
    let limit = expected_bytes.saturating_add(1);
    let mut bytes = Vec::with_capacity(expected_bytes.min(1024 * 1024));
    // A large body is read in slices so a view change abandons it mid-transfer instead of
    // after the last byte.
    while bytes.len() < limit {
        if cancellation.is_some_and(|keep_going| !keep_going()) {
            return Err(RemoteError::Cancelled);
        }
        let slice = (limit - bytes.len()).min(BODY_READ_SLICE_BYTES) as u64;
        if response.by_ref().take(slice).read_to_end(&mut bytes)? == 0 {
            break;
        }
    }
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

const RETRY_BASE_DELAY: Duration = Duration::from_millis(100);
/// Longest wait a server can impose through `Retry-After` before the request is retried anyway;
/// an interactive viewer must not park a worker for minutes on a server's say-so.
const MAXIMUM_RETRY_AFTER: Duration = Duration::from_secs(5);

/// Exponential backoff with jitter (between one and two times `100 ms * 2^attempt`), raised to
/// the server's `Retry-After` when that is longer. Jitter keeps the many readers of one session
/// from retrying a recovering server in lockstep.
fn retry_delay(attempt: usize, retry_after: Option<Duration>) -> Duration {
    let base = RETRY_BASE_DELAY.saturating_mul(1_u32 << attempt.min(5));
    let base_millis = u64::try_from(base.as_millis()).unwrap_or(u64::MAX);
    // `RandomState` is seeded per instance from process-level randomness.
    let random = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    let jittered = base.saturating_add(Duration::from_millis(random % base_millis.max(1)));
    jittered.max(retry_after.unwrap_or_default().min(MAXIMUM_RETRY_AFTER))
}

/// Parses the delay-seconds form of `Retry-After`. The HTTP-date form is ignored (plain
/// backoff applies) rather than trusted against a possibly skewed local clock.
fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Sleeps in short slices so a cancelled generation does not sit out a full backoff.
fn sleep_unless_cancelled(delay: Duration, cancellation: Option<&RemoteCancellation>) {
    const SLICE: Duration = Duration::from_millis(20);
    let mut remaining = delay;
    while !remaining.is_zero() {
        if cancellation.is_some_and(|keep_going| !keep_going()) {
            return;
        }
        let slice = remaining.min(SLICE);
        thread::sleep(slice);
        remaining = remaining.saturating_sub(slice);
    }
}

impl Read for HttpRangeReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.check_cancellation().map_err(io::Error::other)?;
        if buffer.is_empty() || self.position >= self.length {
            return Ok(0);
        }
        if let Some(count) = self.read_planned(buffer)? {
            return Ok(count);
        }
        self.current_extent = None;
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

/// Steers a remote reader after it has been moved into a format decoder that exposes no
/// mutable access to it (bigWig/bigBed). Changes take effect at the reader's next read.
#[derive(Clone, Default)]
pub(crate) struct RemoteReaderControl(Arc<std::sync::Mutex<RemoteReaderControlState>>);

#[derive(Default)]
struct RemoteReaderControlState {
    cancellation: PendingChange<RemoteCancellation>,
    transfer_budget: PendingChange<RemoteTransferBudget>,
    stats: RemoteIoStats,
}

/// A setting change waiting for the reader's next read.
#[derive(Default)]
enum PendingChange<T> {
    #[default]
    Unchanged,
    Set(Option<T>),
}

impl RemoteReaderControl {
    fn state(&self) -> std::sync::MutexGuard<'_, RemoteReaderControlState> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn set_cancellation(&self, cancellation: Option<RemoteCancellation>) {
        self.state().cancellation = PendingChange::Set(cancellation);
    }

    pub(crate) fn set_transfer_budget(&self, budget: Option<RemoteTransferBudget>) {
        self.state().transfer_budget = PendingChange::Set(budget);
    }

    /// Transfer statistics as of the reader's most recent read.
    pub(crate) fn stats(&self) -> RemoteIoStats {
        self.state().stats
    }
}

/// A [`HttpRangeReader`] that applies [`RemoteReaderControl`] changes before each read.
pub(crate) struct ControlledRangeReader {
    reader: HttpRangeReader,
    control: RemoteReaderControl,
}

impl ControlledRangeReader {
    pub(crate) fn open(
        url: &str,
        config: HttpRangeConfig,
    ) -> Result<(Self, RemoteReaderControl), RemoteError> {
        let reader = HttpRangeReader::open(url, config)?;
        let control = RemoteReaderControl::default();
        control.state().stats = reader.stats();
        Ok((
            Self {
                reader,
                control: control.clone(),
            },
            control,
        ))
    }
}

impl Read for ControlledRangeReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let (cancellation, transfer_budget) = {
            let mut state = self.control.state();
            (
                std::mem::take(&mut state.cancellation),
                std::mem::take(&mut state.transfer_budget),
            )
        };
        if let PendingChange::Set(cancellation) = cancellation {
            self.reader.set_cancellation(cancellation);
        }
        if let PendingChange::Set(transfer_budget) = transfer_budget {
            self.reader.set_transfer_budget(transfer_budget);
        }
        let result = self.reader.read(buffer);
        self.control.state().stats = self.reader.stats();
        result
    }
}

impl Seek for ControlledRangeReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.reader.seek(position)
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
    #[error("remote speculative transfer reached its request or response-byte budget")]
    TransferBudgetExceeded,
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
        /// Inclusive byte ranges served, in request order (the opening probe is `(0, 0)`).
        ranges: Arc<std::sync::Mutex<Vec<(usize, usize)>>>,
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
            let ranges = Arc::new(std::sync::Mutex::new(Vec::new()));
            let worker_ranges = Arc::clone(&ranges);
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
                    worker_ranges.lock().unwrap().push(range);
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
                ranges,
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
    fn authenticated_redirects_cannot_leave_the_opening_origin() {
        for bearer in [false, true] {
            for redirect_probe in [false, true] {
                let origin = Server::http("127.0.0.1:0").unwrap();
                let destination = Server::http("127.0.0.1:0").unwrap();
                let url = format!("http://{}/object?token=private-token", origin.server_addr());
                let location = format!("http://{}/elsewhere", destination.server_addr());
                let worker = thread::spawn(move || {
                    if !redirect_probe {
                        origin
                            .recv()
                            .unwrap()
                            .respond(
                                Response::from_data(vec![0])
                                    .with_status_code(StatusCode(206))
                                    .with_header(test_header("Content-Range", "bytes 0-0/32")),
                            )
                            .unwrap();
                    }
                    // A same-origin hop must not authorize the next foreign origin.
                    origin
                        .recv()
                        .unwrap()
                        .respond(
                            Response::empty(StatusCode(302))
                                .with_header(test_header("Location", "/hop")),
                        )
                        .unwrap();
                    origin
                        .recv()
                        .unwrap()
                        .respond(
                            Response::empty(StatusCode(302))
                                .with_header(test_header("Location", location)),
                        )
                        .unwrap();
                });
                let config = HttpRangeConfig {
                    maximum_retries: 0,
                    authentication: if bearer {
                        RemoteAuthentication::Bearer("private-key".into())
                    } else {
                        RemoteAuthentication::Header {
                            name: HeaderName::from_static("x-api-key"),
                            value: HeaderValue::from_static("private-key"),
                        }
                    },
                    ..HttpRangeConfig::default()
                };
                let message = if redirect_probe {
                    HttpRangeReader::open(&url, config)
                        .err()
                        .unwrap()
                        .to_string()
                } else {
                    HttpRangeReader::open(&url, config)
                        .unwrap()
                        .read_exact(&mut [0; 8])
                        .unwrap_err()
                        .to_string()
                };
                worker.join().unwrap();
                assert!(destination.try_recv().unwrap().is_none());
                assert!(!message.contains("private-key"));
                assert!(!message.contains("private-token"));
            }
        }
        assert_eq!(
            Url::parse("https://example.org/object").unwrap().origin(),
            Url::parse("https://example.org:443/hop").unwrap().origin()
        );
        assert_ne!(
            Url::parse("https://example.org/object").unwrap().origin(),
            Url::parse("http://example.org/hop").unwrap().origin()
        );
    }

    #[test]
    fn read_ahead_is_capped_to_cache_capacity_even_below_its_minimum() {
        for minimum in [1, 4] {
            for length in [32, 35] {
                let data = (0..length).collect::<Vec<u8>>();
                let fixture = RangeFixture::start(data.clone()).unwrap();
                let mut reader = HttpRangeReader::open(
                    &fixture.url,
                    HttpRangeConfig {
                        chunk_size: NonZeroUsize::new(8).unwrap(),
                        maximum_cache_bytes: NonZeroUsize::new(8).unwrap(),
                        minimum_coalesced_chunks: NonZeroUsize::new(minimum).unwrap(),
                        maximum_coalesced_chunks: NonZeroUsize::new(4).unwrap(),
                        ..HttpRangeConfig::default()
                    },
                )
                .unwrap();
                let mut bytes = vec![0; usize::from(length)];
                reader.read_exact(&mut bytes).unwrap();
                assert_eq!(bytes, data);
                assert_eq!(reader.read(&mut [0; 1]).unwrap(), 0);
                reader.seek(SeekFrom::Start(3)).unwrap();
                reader.read_exact(&mut bytes[..12]).unwrap();
                assert_eq!(bytes[..12], data[3..15]);
                assert_eq!(reader.blocks.len(), 1);
                assert!(reader.stats().evictions > 0);
                assert!(reader.current_block.as_ref().unwrap().1.len() <= 8);
            }
        }
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
    fn transfer_budget_stops_speculative_ranges_before_the_hard_limit() {
        let Some(fixture) = RangeFixture::start((0_u8..=255).cycle().take(4_096).collect()) else {
            return;
        };
        let mut reader = HttpRangeReader::open(
            &fixture.url,
            HttpRangeConfig {
                chunk_size: NonZeroUsize::new(256).unwrap(),
                maximum_cache_bytes: NonZeroUsize::new(1_024).unwrap(),
                minimum_coalesced_chunks: NonZeroUsize::MIN,
                maximum_coalesced_chunks: NonZeroUsize::MIN,
                ..HttpRangeConfig::default()
            },
        )
        .unwrap();
        let baseline = reader.stats();
        reader.set_transfer_budget(Some(RemoteTransferBudget {
            maximum_requests: 1,
            maximum_response_bytes: 256,
        }));
        let mut first = [0_u8; 1];
        reader.read_exact(&mut first).unwrap();
        reader.seek(SeekFrom::Start(512)).unwrap();
        let error = reader.read_exact(&mut first).unwrap_err();
        assert!(error.to_string().contains("transfer reached"));
        let delta = reader.stats();
        assert_eq!(delta.requests.saturating_sub(baseline.requests), 1);
        assert_eq!(
            delta.response_bytes.saturating_sub(baseline.response_bytes),
            256
        );
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

    fn fixture_bytes(length: usize) -> Vec<u8> {
        (0..length)
            .map(|index| (index % 251) as u8 ^ (index / 251) as u8)
            .collect()
    }

    #[test]
    fn read_plan_fetches_the_planned_bytes_and_coalesces_near_ranges() {
        let data = fixture_bytes(1024 * 1024);
        let Some(fixture) = RangeFixture::start(data.clone()) else {
            return;
        };
        let mut reader = HttpRangeReader::open(
            &fixture.url,
            HttpRangeConfig {
                chunk_size: NonZeroUsize::new(4_096).unwrap(),
                maximum_cache_bytes: NonZeroUsize::new(256 * 1024).unwrap(),
                minimum_coalesced_chunks: NonZeroUsize::new(4).unwrap(),
                maximum_coalesced_chunks: NonZeroUsize::new(4).unwrap(),
                ..HttpRangeConfig::default()
            },
        )
        .unwrap();
        let opened = reader.stats();
        // Two ranges 100 bytes apart become one request; the distant one is its own.
        reader.set_read_plan([500_000..500_200, 10_600..11_000, 10_000..10_500]);
        let read_at = |reader: &mut HttpRangeReader, start: usize, length: usize| {
            let mut buffer = vec![0; length];
            reader.seek(SeekFrom::Start(start as u64)).unwrap();
            reader.read_exact(&mut buffer).unwrap();
            assert_eq!(buffer, data[start..start + length], "bytes at {start}");
        };
        read_at(&mut reader, 10_000, 500);
        read_at(&mut reader, 10_600, 400);
        read_at(&mut reader, 500_000, 200);
        assert_eq!(
            fixture.ranges.lock().unwrap()[1..],
            [(10_000, 10_999), (500_000, 500_199)]
        );
        // Accounting is exact: 1,200 bytes where the block cache would have fetched two
        // 16 KiB windows.
        let planned = reader.stats();
        assert_eq!(planned.requests - opened.requests, 2);
        assert_eq!(planned.response_bytes - opened.response_bytes, 1_200);
        assert_eq!(planned.cache_misses - opened.cache_misses, 2);

        // Revisiting planned bytes is free, also under a later plan that no longer lists them.
        reader.set_read_plan(std::iter::once(700_000..700_010));
        read_at(&mut reader, 10_250, 600);
        read_at(&mut reader, 500_100, 100);
        assert_eq!(reader.stats().requests, planned.requests);
        assert_eq!(reader.stats().cache_hits - planned.cache_hits, 2);

        // A read outside every planned range keeps the block cache's aligned read-ahead.
        read_at(&mut reader, 300_000, 10);
        assert_eq!(
            fixture.ranges.lock().unwrap().last().copied(),
            Some((299_008, 299_008 + 4 * 4_096 - 1))
        );
    }

    #[test]
    fn a_long_planned_range_is_fetched_in_a_few_growing_requests() {
        let data = fixture_bytes(1024 * 1024);
        let Some(fixture) = RangeFixture::start(data.clone()) else {
            return;
        };
        let mut reader = HttpRangeReader::open(
            &fixture.url,
            HttpRangeConfig {
                chunk_size: NonZeroUsize::new(4_096).unwrap(),
                maximum_cache_bytes: NonZeroUsize::new(512 * 1024).unwrap(),
                minimum_coalesced_chunks: NonZeroUsize::new(4).unwrap(),
                maximum_coalesced_chunks: NonZeroUsize::new(4).unwrap(),
                ..HttpRangeConfig::default()
            },
        )
        .unwrap();
        let opened = reader.stats();
        reader.set_read_plan(std::iter::once(100_000..300_000));
        reader.seek(SeekFrom::Start(100_000)).unwrap();
        let mut buffer = vec![0; 200_000];
        reader.read_exact(&mut buffer).unwrap();
        assert_eq!(buffer, data[100_000..300_000]);
        // One 16 KiB window, then doubling to the 64 KiB limit: five requests, not the
        // thirteen windows (or forty-nine blocks) of unplanned reading.
        assert_eq!(
            fixture.ranges.lock().unwrap()[1..],
            [
                (100_000, 116_383),
                (116_384, 149_151),
                (149_152, 214_687),
                (214_688, 280_223),
                (280_224, 299_999),
            ]
        );
        assert_eq!(
            reader.stats().response_bytes - opened.response_bytes,
            200_000
        );

        // A transfer budget shortens the request instead of overshooting it.
        reader.set_read_plan(std::iter::once(400_000..500_000));
        reader.set_transfer_budget(Some(RemoteTransferBudget {
            maximum_requests: 8,
            maximum_response_bytes: 1_000,
        }));
        reader.seek(SeekFrom::Start(400_000)).unwrap();
        reader.read_exact(&mut buffer[..1_000]).unwrap();
        assert_eq!(buffer[..1_000], data[400_000..401_000]);
        let error = reader.read_exact(&mut buffer[..1]).unwrap_err();
        assert!(error.to_string().contains("transfer reached"));
        assert_eq!(
            fixture.ranges.lock().unwrap().last().copied(),
            Some((400_000, 400_999))
        );
    }

    #[test]
    fn planned_extents_are_evicted_at_the_memory_bound() {
        let data = fixture_bytes(64 * 1024);
        let Some(fixture) = RangeFixture::start(data.clone()) else {
            return;
        };
        let mut reader = HttpRangeReader::open(
            &fixture.url,
            HttpRangeConfig {
                chunk_size: NonZeroUsize::new(256).unwrap(),
                maximum_cache_bytes: NonZeroUsize::new(2_048).unwrap(),
                maximum_coalesced_chunks: NonZeroUsize::new(4).unwrap(),
                ..HttpRangeConfig::default()
            },
        )
        .unwrap();
        reader.set_read_plan([1_000..2_000, 10_000..11_000, 20_000..21_000]);
        let mut buffer = vec![0; 1_000];
        for start in [1_000_usize, 10_000, 20_000] {
            reader.seek(SeekFrom::Start(start as u64)).unwrap();
            reader.read_exact(&mut buffer).unwrap();
            assert_eq!(buffer, data[start..start + 1_000]);
        }
        assert_eq!(reader.stats().evictions, 1);
        assert!(reader.extent_bytes <= 2_048);
        assert_eq!(
            reader.extents.keys().copied().collect::<Vec<_>>(),
            [10_000, 20_000]
        );
        // The evicted range is fetched again; the retained ones are not.
        let requests = reader.stats().requests;
        reader.seek(SeekFrom::Start(20_000)).unwrap();
        reader.read_exact(&mut buffer).unwrap();
        assert_eq!(reader.stats().requests, requests);
        reader.seek(SeekFrom::Start(1_000)).unwrap();
        reader.read_exact(&mut buffer).unwrap();
        assert_eq!(reader.stats().requests, requests + 1);
    }

    #[test]
    fn recently_used_blocks_are_served_from_memory_not_reread_from_disk() {
        let data = fixture_bytes(4_096);
        let Some(fixture) = RangeFixture::start(data.clone()) else {
            return;
        };
        let mut reader = HttpRangeReader::open(
            &fixture.url,
            HttpRangeConfig {
                chunk_size: NonZeroUsize::new(256).unwrap(),
                maximum_cache_bytes: NonZeroUsize::new(1_024).unwrap(),
                maximum_coalesced_chunks: NonZeroUsize::new(4).unwrap(),
                ..HttpRangeConfig::default()
            },
        )
        .unwrap();
        let mut buffer = vec![0; 1_024];
        reader.read_exact(&mut buffer).unwrap();
        let requests = reader.stats().requests;
        // With the disk copies gone, only the in-memory tier can serve a block switch.
        for entry in fs::read_dir(reader.cache_directory.path()).unwrap() {
            fs::remove_file(entry.unwrap().path()).unwrap();
        }
        for start in [0_usize, 768, 256, 512, 0] {
            reader.seek(SeekFrom::Start(start as u64)).unwrap();
            reader.read_exact(&mut buffer[..256]).unwrap();
            assert_eq!(buffer[..256], data[start..start + 256]);
        }
        assert_eq!(reader.stats().requests, requests);
        assert_eq!(fixture.requests.load(Ordering::Relaxed), requests);
        // The memory tier never outgrows the configured cache.
        assert!(reader.memory_blocks.len() <= 4);
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

    #[test]
    fn a_body_that_breaks_off_mid_transfer_is_retried() {
        use std::{
            io::{BufRead, BufReader, Write},
            net::TcpListener,
        };

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let data = (0_u8..64).collect::<Vec<_>>();
        let served = data.clone();
        // Request 1 is the probe, request 2 promises eight bytes and hangs up after three,
        // request 3 is the retry.
        let worker = thread::spawn(move || {
            for request_number in 1..=3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut range = None;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("range:") {
                        range = parse_test_range(value.trim(), served.len());
                    }
                }
                let (start, end) = range.unwrap();
                let body = &served[start..=end];
                write!(
                    stream,
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{}\r\n\
                     Content-Length: {}\r\nETag: \"broken-body\"\r\nConnection: close\r\n\r\n",
                    served.len(),
                    body.len(),
                )
                .unwrap();
                let sent = if request_number == 2 { 3 } else { body.len() };
                stream.write_all(&body[..sent]).unwrap();
            }
        });
        let mut reader = HttpRangeReader::open(
            &format!("http://{address}/broken.bin"),
            HttpRangeConfig {
                chunk_size: NonZeroUsize::new(8).unwrap(),
                maximum_cache_bytes: NonZeroUsize::new(16).unwrap(),
                maximum_coalesced_chunks: NonZeroUsize::MIN,
                maximum_retries: 1,
                ..HttpRangeConfig::default()
            },
        )
        .unwrap();
        let mut bytes = [0_u8; 8];
        reader.read_exact(&mut bytes).unwrap();
        worker.join().unwrap();
        assert_eq!(bytes, data[..8]);
        assert_eq!(reader.stats().requests, 3);
        assert_eq!(reader.stats().retries, 1);
        assert_eq!(reader.stats().response_bytes, 9);
    }

    #[test]
    fn retry_delay_is_jittered_backoff_bounded_by_retry_after() {
        for _ in 0..64 {
            let first = retry_delay(0, None);
            assert!((Duration::from_millis(100)..Duration::from_millis(200)).contains(&first));
            let second = retry_delay(1, None);
            assert!((Duration::from_millis(200)..Duration::from_millis(400)).contains(&second));
        }
        assert_eq!(
            retry_delay(0, Some(Duration::from_secs(3))),
            Duration::from_secs(3)
        );
        // A server cannot park the reader for longer than the cap.
        assert_eq!(
            retry_delay(0, Some(Duration::from_secs(600))),
            MAXIMUM_RETRY_AFTER
        );
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(parse_retry_after(&headers), None);
        headers.insert(RETRY_AFTER, HeaderValue::from_static(" 7 "));
        assert_eq!(parse_retry_after(&headers), Some(Duration::from_secs(7)));
        headers.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2026 07:28:00 GMT"),
        );
        assert_eq!(parse_retry_after(&headers), None);
    }

    #[test]
    fn retry_backoff_ends_early_when_the_generation_is_cancelled() {
        let cancellation: RemoteCancellation = Arc::new(|| false);
        let started = std::time::Instant::now();
        sleep_unless_cancelled(Duration::from_secs(5), Some(&cancellation));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn if_range_never_carries_a_weak_entity_tag() {
        let validator =
            |etag: Option<&'static str>, last_modified: Option<&'static str>| Validator {
                etag: etag.map(HeaderValue::from_static),
                last_modified: last_modified.map(HeaderValue::from_static),
            };
        let date = "Wed, 21 Oct 2026 07:28:00 GMT";
        assert_eq!(
            validator(Some("\"strong\""), Some(date)).if_range(),
            Some(HeaderValue::from_static("\"strong\""))
        );
        assert_eq!(
            validator(Some("W/\"weak\""), Some(date)).if_range(),
            Some(HeaderValue::from_static(date))
        );
        assert_eq!(validator(Some("W/\"weak\""), None).if_range(), None);
        assert_eq!(validator(None, None).if_range(), None);
    }

    #[test]
    fn weak_entity_tags_do_not_turn_range_reads_into_changed_object_errors() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let data = (0_u8..64).collect::<Vec<_>>();
            while !worker_stop.load(Ordering::Relaxed) {
                let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(20)) else {
                    continue;
                };
                // RFC 9110: a weak validator in If-Range never matches, so the whole
                // representation is sent with 200.
                let weak_if_range = request.headers().iter().any(|header| {
                    header.field.equiv("If-Range") && header.value.as_str().starts_with("W/")
                });
                if weak_if_range {
                    request
                        .respond(
                            Response::from_data(data.clone())
                                .with_header(test_header("ETag", "W/\"weak-fixture\"")),
                        )
                        .unwrap();
                    continue;
                }
                let (start, end) = request
                    .headers()
                    .iter()
                    .find(|header| header.field.equiv("Range"))
                    .and_then(|header| parse_test_range(header.value.as_str(), data.len()))
                    .unwrap();
                request
                    .respond(
                        Response::from_data(data[start..=end].to_vec())
                            .with_status_code(StatusCode(206))
                            .with_header(test_header(
                                "Content-Range",
                                format!("bytes {start}-{end}/{}", data.len()),
                            ))
                            .with_header(test_header("ETag", "W/\"weak-fixture\"")),
                    )
                    .unwrap();
            }
        });
        let mut reader = HttpRangeReader::open(
            &format!("http://{address}/weak.bin"),
            HttpRangeConfig {
                chunk_size: NonZeroUsize::new(8).unwrap(),
                maximum_cache_bytes: NonZeroUsize::new(16).unwrap(),
                ..HttpRangeConfig::default()
            },
        )
        .unwrap();
        let mut bytes = [0_u8; 8];
        let result = reader.read_exact(&mut bytes);
        stop.store(true, Ordering::Relaxed);
        worker.join().unwrap();
        result.unwrap();
        assert_eq!(bytes, [0, 1, 2, 3, 4, 5, 6, 7]);
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
