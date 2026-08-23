use std::{
    fs,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use reqwest::{
    blocking::Client,
    header::{CONTENT_RANGE, RANGE},
};
use tiny_http::{Header, Method, Response, Server, StatusCode};

const RANGE_CHUNK_SIZE: u64 = 256 * 1024;

pub struct RangeServer {
    url: String,
    stop: Arc<AtomicBool>,
    requests: Arc<AtomicU64>,
    bytes: Arc<AtomicU64>,
    htslib_index_cache: PathBuf,
    thread: Option<thread::JoinHandle<()>>,
}

struct ServedFiles {
    bam: Arc<Vec<u8>>,
    index: Arc<Vec<u8>>,
    bam_route: Arc<String>,
    index_route: Arc<String>,
    requests: Arc<AtomicU64>,
    bytes: Arc<AtomicU64>,
}

impl RangeServer {
    pub fn start(bam_path: &Path, latency_ms: u64) -> Result<Self> {
        let bam = Arc::new(
            fs::read(bam_path).with_context(|| format!("failed to load {}", bam_path.display()))?,
        );
        let index_path = Path::new(&format!("{}.bai", bam_path.display())).to_path_buf();
        let index = Arc::new(
            fs::read(&index_path)
                .with_context(|| format!("failed to load {}", index_path.display()))?,
        );
        let server = Server::http("127.0.0.1:0").map_err(|error| anyhow::anyhow!("{error}"))?;
        let address = server
            .server_addr()
            .to_ip()
            .context("HTTP server did not bind an IP address")?;
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let bam_name = format!("rgv-benchmark-{}-{nonce}.bam", std::process::id());
        let bam_route = Arc::new(format!("/{bam_name}"));
        let index_route = Arc::new(format!("/{bam_name}.bai"));
        let url = format!("http://{address}{bam_route}");
        let htslib_index_cache = std::env::current_dir()?.join(format!("{bam_name}.bai"));
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicU64::new(0));
        let bytes = Arc::new(AtomicU64::new(0));

        let server_stop = Arc::clone(&stop);
        let server_requests = Arc::clone(&requests);
        let server_bytes = Arc::clone(&bytes);
        let served_files = Arc::new(ServedFiles {
            bam,
            index,
            bam_route,
            index_route,
            requests: server_requests,
            bytes: server_bytes,
        });
        let latency = Duration::from_millis(latency_ms);
        let handle = thread::spawn(move || {
            while !server_stop.load(Ordering::Relaxed) {
                let request = match server.recv_timeout(Duration::from_millis(25)) {
                    Ok(Some(request)) => request,
                    Ok(None) => continue,
                    Err(_) => break,
                };
                let request_files = Arc::clone(&served_files);
                thread::spawn(move || {
                    handle_request(request, request_files, latency);
                });
            }
        });

        Ok(Self {
            url,
            stop,
            requests,
            bytes,
            htslib_index_cache,
            thread: Some(handle),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn reset_counters(&self) {
        self.requests.store(0, Ordering::Relaxed);
        self.bytes.store(0, Ordering::Relaxed);
    }

    pub fn counters(&self) -> (u64, u64) {
        (
            self.requests.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed),
        )
    }
}

fn handle_request(request: tiny_http::Request, files: Arc<ServedFiles>, latency: Duration) {
    files.requests.fetch_add(1, Ordering::Relaxed);
    #[cfg(debug_assertions)]
    eprintln!(
        "range-server {} {} {:?}",
        request.method(),
        request.url(),
        request
            .headers()
            .iter()
            .find(|header| header.field.equiv("Range"))
            .map(|header| header.value.as_str())
    );
    if !latency.is_zero() {
        thread::sleep(latency);
    }

    let data = if request.url() == files.bam_route.as_str() {
        Arc::clone(&files.bam)
    } else if request.url() == files.index_route.as_str() {
        Arc::clone(&files.index)
    } else {
        let _ = request.respond(Response::empty(StatusCode(404)));
        return;
    };

    let length_header = header("Content-Length", data.len().to_string());
    let accept_ranges = header("Accept-Ranges", "bytes");
    if request.method() == &Method::Head {
        let response = Response::empty(StatusCode(200))
            .with_header(length_header)
            .with_header(accept_ranges);
        let _ = request.respond(response);
        return;
    }

    let requested_range = request
        .headers()
        .iter()
        .find(|candidate| candidate.field.equiv("Range"))
        .and_then(|candidate| parse_range(candidate.value.as_str(), data.len()));

    let (start, end, status) = requested_range
        .map(|(start, end)| (start, end, StatusCode(206)))
        .unwrap_or((0, data.len().saturating_sub(1), StatusCode(200)));
    let body = if data.is_empty() {
        Vec::new()
    } else {
        data[start..=end].to_vec()
    };
    files.bytes.fetch_add(body.len() as u64, Ordering::Relaxed);

    let mut response = Response::from_data(body)
        .with_status_code(status)
        .with_header(accept_ranges);
    if status == StatusCode(206) {
        response.add_header(header(
            "Content-Range",
            format!("bytes {start}-{end}/{}", data.len()),
        ));
    }
    let _ = request.respond(response);
}

impl Drop for RangeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
        if self.htslib_index_cache.exists() {
            let _ = fs::remove_file(&self.htslib_index_cache);
        }
    }
}

pub struct HttpRangeReader {
    client: Client,
    url: String,
    len: u64,
    position: u64,
    cache_start: u64,
    cache: Vec<u8>,
}

impl HttpRangeReader {
    pub fn new(url: impl Into<String>) -> Result<Self> {
        let url = url.into();
        let client = Client::builder().build()?;
        let response = client
            .get(&url)
            .header(RANGE, "bytes=0-0")
            .send()?
            .error_for_status()?;
        let content_range = response
            .headers()
            .get(CONTENT_RANGE)
            .context("remote BAM range response lacked Content-Range")?
            .to_str()?;
        let len = content_range
            .rsplit_once('/')
            .context("invalid remote BAM Content-Range")?
            .1
            .parse()?;
        Ok(Self {
            client,
            url,
            len,
            position: 0,
            cache_start: 0,
            cache: Vec::new(),
        })
    }

    fn refill(&mut self) -> io::Result<()> {
        let start = (self.position / RANGE_CHUNK_SIZE) * RANGE_CHUNK_SIZE;
        let end = (start + RANGE_CHUNK_SIZE).min(self.len).saturating_sub(1);
        let response = self
            .client
            .get(&self.url)
            .header(RANGE, format!("bytes={start}-{end}"))
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(io::Error::other)?;
        self.cache = response.bytes().map_err(io::Error::other)?.to_vec();
        self.cache_start = start;
        Ok(())
    }
}

impl Read for HttpRangeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.position >= self.len {
            return Ok(0);
        }
        let cache_end = self.cache_start + self.cache.len() as u64;
        if self.cache.is_empty() || self.position < self.cache_start || self.position >= cache_end {
            self.refill()?;
        }
        let offset = (self.position - self.cache_start) as usize;
        let available = self.cache.len() - offset;
        let remaining = (self.len - self.position) as usize;
        let count = buf.len().min(available).min(remaining);
        buf[..count].copy_from_slice(&self.cache[offset..offset + count]);
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for HttpRangeReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let next = match position {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::End(offset) => i128::from(self.len) + i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        if !(0..=i128::from(self.len)).contains(&next) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek outside remote object",
            ));
        }
        self.position = next as u64;
        Ok(self.position)
    }
}

fn parse_range(value: &str, len: usize) -> Option<(usize, usize)> {
    let value = value.strip_prefix("bytes=")?;
    let (start, end) = value.split_once('-')?;
    let start: usize = start.parse().ok()?;
    let end = if end.is_empty() {
        len.checked_sub(1)?
    } else {
        end.parse::<usize>().ok()?.min(len.checked_sub(1)?)
    };
    (start <= end && end < len).then_some((start, end))
}

fn header(name: &str, value: impl Into<String>) -> Header {
    Header::from_bytes(name.as_bytes(), value.into().as_bytes()).expect("valid HTTP header")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bounded_and_open_ended_ranges() {
        assert_eq!(parse_range("bytes=5-9", 20), Some((5, 9)));
        assert_eq!(parse_range("bytes=5-", 20), Some((5, 19)));
        assert_eq!(parse_range("bytes=5-99", 20), Some((5, 19)));
        assert_eq!(parse_range("items=5-9", 20), None);
        assert_eq!(parse_range("bytes=20-21", 20), None);
    }
}
