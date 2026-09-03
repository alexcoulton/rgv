use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use rgv_alignment_scene::{ReferenceRevision, RevisionFingerprint};
use rgv_core::{GenerationClock, GenomicInterval, PanelGeneration, ReferenceDictionary};
use rgv_hts::{
    FastaOpenOptions, FastaReference, ReferenceSlice, RemoteCancellation, RemoteIoStats,
};

use crate::{
    EngineQueueError,
    mailbox::{CoalescingReceiver, CoalescingSender, bounded_coalescing},
};

/// Maximum number of bases fetched by one underlying FASTA operation.
///
/// A logical scene authority may be wider. The reference actor assembles it from bounded chunks
/// and checks cancellation between them, so consumers still receive one atomic slice without a
/// broad horizon becoming one uninterruptible local or remote source operation.
pub const MAX_REFERENCE_QUERY_BASES: u64 = 1_000_000;

/// Complete identity of one reference query. Panel generation alone is not sufficient because
/// one scene-camera generation may request several successive buffered horizons. Carrying the
/// expected open-instance revision and exact interval lets consumers reject an older completion
/// or failure without consulting mutable side state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceQueryIdentity {
    request: PanelGeneration,
    revision: ReferenceRevision,
    interval: GenomicInterval,
}

impl ReferenceQueryIdentity {
    #[must_use]
    pub const fn new(
        request: PanelGeneration,
        revision: ReferenceRevision,
        interval: GenomicInterval,
    ) -> Self {
        Self {
            request,
            revision,
            interval,
        }
    }

    #[must_use]
    pub const fn request(self) -> PanelGeneration {
        self.request
    }

    #[must_use]
    pub const fn revision(self) -> ReferenceRevision {
        self.revision
    }

    #[must_use]
    pub const fn interval(self) -> GenomicInterval {
        self.interval
    }
}

/// Process-local identity for the next successfully opened reference authority. Reference scene
/// caches are process-local, so an open-instance revision is sufficient to prevent two live or
/// successively opened FASTA handles from sharing semantic pages accidentally. This is
/// deliberately not presented as a content hash.
static NEXT_REFERENCE_OPEN_INSTANCE: AtomicU64 = AtomicU64::new(1);
const REFERENCE_OPEN_INSTANCE_DOMAIN: u64 = u64::from_le_bytes(*b"RGVREF01");

enum ReferenceEngineRequest {
    Open {
        path: PathBuf,
        options: FastaOpenOptions,
    },
    Query {
        query: ReferenceQueryIdentity,
        clock: GenerationClock,
        contig_name: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReferenceEngineEvent {
    Opened {
        revision: ReferenceRevision,
        dictionary: ReferenceDictionary,
        remote_io_stats: Option<RemoteIoStats>,
    },
    QueryCompleted {
        query: ReferenceQueryIdentity,
        slice: Arc<ReferenceSlice>,
        remote_io_stats: Option<RemoteIoStats>,
    },
    Error {
        /// `None` identifies an open failure; every query failure carries its complete identity.
        query: Option<ReferenceQueryIdentity>,
        message: String,
    },
}

/// Single-owner FASTA actor. Opening an index and fetching sequence never blocks the UI thread.
pub struct ReferenceEngine {
    input: Option<Sender<ReferenceEngineRequest>>,
    output: CoalescingReceiver<ReferenceEngineEvent>,
    worker: Option<JoinHandle<()>>,
}

impl ReferenceEngine {
    #[must_use]
    pub fn spawn(queue_capacity: usize) -> Self {
        let capacity = queue_capacity.max(1);
        let (input_tx, input_rx) = bounded(capacity);
        let (output_tx, output_rx) = bounded_coalescing(capacity);
        let worker = thread::Builder::new()
            .name("rgv-reference-actor".to_owned())
            .spawn(move || worker_loop(&input_rx, &output_tx))
            .expect("failed to spawn RGV reference actor");
        Self {
            input: Some(input_tx),
            output: output_rx,
            worker: Some(worker),
        }
    }

    pub fn try_open(&self, path: PathBuf) -> Result<(), EngineQueueError> {
        self.try_open_with_options(path, FastaOpenOptions::default())
    }

    pub fn try_open_with_options(
        &self,
        path: PathBuf,
        options: FastaOpenOptions,
    ) -> Result<(), EngineQueueError> {
        self.try_send(ReferenceEngineRequest::Open { path, options })
    }

    pub fn try_query(
        &self,
        query: ReferenceQueryIdentity,
        clock: GenerationClock,
        contig_name: String,
    ) -> Result<(), EngineQueueError> {
        self.try_send(ReferenceEngineRequest::Query {
            query,
            clock,
            contig_name,
        })
    }

    pub fn try_receive(&self) -> Result<Option<ReferenceEngineEvent>, EngineQueueError> {
        self.output
            .try_receive()
            .map_err(|_| EngineQueueError::Disconnected)
    }

    fn try_send(&self, request: ReferenceEngineRequest) -> Result<(), EngineQueueError> {
        let Some(input) = &self.input else {
            return Err(EngineQueueError::Disconnected);
        };
        input.try_send(request).map_err(|error| match error {
            TrySendError::Full(_) => EngineQueueError::Full,
            TrySendError::Disconnected(_) => EngineQueueError::Disconnected,
        })
    }
}

impl Drop for ReferenceEngine {
    fn drop(&mut self) {
        self.input.take();
        self.output.close();
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::warn!("reference actor panicked while shutting down");
        }
    }
}

fn worker_loop(
    input: &Receiver<ReferenceEngineRequest>,
    output: &CoalescingSender<ReferenceEngineEvent>,
) {
    let mut reference: Option<(FastaReference, ReferenceRevision)> = None;
    while let Ok(request) = input.recv() {
        let event = match request {
            ReferenceEngineRequest::Open { path, options } => {
                match FastaReference::open_with_options(path, &options) {
                    Ok(opened) => {
                        let dictionary = opened.reference_dictionary().clone();
                        let remote_io_stats = opened.remote_io_stats();
                        if let Some(revision) = next_reference_open_instance_revision() {
                            reference = Some((opened, revision));
                            ReferenceEngineEvent::Opened {
                                revision,
                                dictionary,
                                remote_io_stats,
                            }
                        } else {
                            ReferenceEngineEvent::Error {
                                query: None,
                                message: "reference open-instance revision authority is exhausted"
                                    .to_owned(),
                            }
                        }
                    }
                    Err(error) => ReferenceEngineEvent::Error {
                        query: None,
                        message: error.to_string(),
                    },
                }
            }
            ReferenceEngineRequest::Query {
                query,
                clock,
                contig_name,
            } => {
                let Some((reference, revision)) = reference.as_ref() else {
                    send_event(
                        output,
                        ReferenceEngineEvent::Error {
                            query: Some(query),
                            message: "reference FASTA is not open".to_owned(),
                        },
                    );
                    continue;
                };
                if query.revision != *revision {
                    continue;
                }
                let query_clock = clock.clone();
                let generation = query.request.generation();
                let cancellation: RemoteCancellation =
                    Arc::new(move || query_clock.is_current(generation));
                reference.set_remote_cancellation(Some(cancellation));
                match fetch_reference_query(reference, &contig_name, query.interval, &clock, query)
                {
                    ReferenceQueryFetch::Completed(slice)
                        if clock.is_current(query.request.generation()) =>
                    {
                        ReferenceEngineEvent::QueryCompleted {
                            query,
                            slice: Arc::new(slice),
                            remote_io_stats: reference.remote_io_stats(),
                        }
                    }
                    ReferenceQueryFetch::MissingContig
                        if clock.is_current(query.request.generation()) =>
                    {
                        ReferenceEngineEvent::Error {
                            query: Some(query),
                            message: format!(
                                "reference FASTA does not contain contig {contig_name}"
                            ),
                        }
                    }
                    ReferenceQueryFetch::Failed(message)
                        if clock.is_current(query.request.generation()) =>
                    {
                        ReferenceEngineEvent::Error {
                            query: Some(query),
                            message,
                        }
                    }
                    ReferenceQueryFetch::Completed(_)
                    | ReferenceQueryFetch::MissingContig
                    | ReferenceQueryFetch::Cancelled
                    | ReferenceQueryFetch::Failed(_) => continue,
                }
            }
        };
        send_event(output, event);
    }
}

enum ReferenceQueryFetch {
    Completed(ReferenceSlice),
    MissingContig,
    Cancelled,
    Failed(String),
}

fn fetch_reference_query(
    reference: &FastaReference,
    contig_name: &str,
    interval: GenomicInterval,
    clock: &GenerationClock,
    query: ReferenceQueryIdentity,
) -> ReferenceQueryFetch {
    let expected_bases = match usize::try_from(interval.len()) {
        Ok(expected) => expected,
        Err(_) => {
            return ReferenceQueryFetch::Failed(
                "reference query length exceeds addressable memory".to_owned(),
            );
        }
    };
    let mut bases = Vec::new();
    if let Err(error) = bases.try_reserve_exact(expected_bases) {
        return ReferenceQueryFetch::Failed(format!(
            "could not reserve memory for reference query: {error}"
        ));
    }

    let mut start = interval.start();
    while start < interval.end() {
        if !clock.is_current(query.request.generation()) {
            return ReferenceQueryFetch::Cancelled;
        }
        let end = start
            .saturating_add(MAX_REFERENCE_QUERY_BASES)
            .min(interval.end());
        let chunk = GenomicInterval::new(interval.contig(), start, end)
            .expect("a bounded reference-query chunk is non-empty");
        let slice = match reference.fetch_slice_by_name(contig_name, chunk) {
            Ok(Some(slice)) => slice,
            Ok(None) => return ReferenceQueryFetch::MissingContig,
            Err(_) if !clock.is_current(query.request.generation()) => {
                return ReferenceQueryFetch::Cancelled;
            }
            Err(error) => return ReferenceQueryFetch::Failed(error.to_string()),
        };
        let expected_chunk_bases = match usize::try_from(chunk.len()) {
            Ok(expected) => expected,
            Err(_) => {
                return ReferenceQueryFetch::Failed(
                    "reference query chunk length exceeds addressable memory".to_owned(),
                );
            }
        };
        if slice.interval != chunk || slice.bases.len() != expected_chunk_bases {
            return ReferenceQueryFetch::Failed(format!(
                "reference source returned {} bases for a {}-base query chunk",
                slice.bases.len(),
                chunk.len()
            ));
        }
        bases.extend_from_slice(&slice.bases);
        start = end;
    }

    if !clock.is_current(query.request.generation()) {
        return ReferenceQueryFetch::Cancelled;
    }
    if bases.len() != expected_bases {
        return ReferenceQueryFetch::Failed(format!(
            "reference source returned {} bases for a {}-base logical query",
            bases.len(),
            interval.len()
        ));
    }
    ReferenceQueryFetch::Completed(ReferenceSlice { interval, bases })
}

fn next_reference_open_instance_revision() -> Option<ReferenceRevision> {
    let sequence = NEXT_REFERENCE_OPEN_INSTANCE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .ok()?;
    Some(ReferenceRevision::new(RevisionFingerprint::new(
        REFERENCE_OPEN_INSTANCE_DOMAIN,
        sequence,
    )))
}

fn send_event(output: &CoalescingSender<ReferenceEngineEvent>, event: ReferenceEngineEvent) {
    let result = if let ReferenceEngineEvent::QueryCompleted { query, .. } = &event {
        output.send_latest_for(query.request.panel_id().get(), event)
    } else {
        output.send_control(event)
    };
    if result.is_err() {
        tracing::debug!("reference event receiver closed");
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use rgv_core::{ContigId, PanelId, ViewportGeneration};

    use super::*;

    #[test]
    fn standalone_reference_opens_and_fetches_by_panel_generation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("genome.fa");
        fs::write(&path, b">chr1\nACGTACGT\n>chr2\nTTAA\n").unwrap();
        fs::write(
            path.with_extension("fa.fai"),
            b"chr1\t8\t6\t8\t9\nchr2\t4\t21\t4\t5\n",
        )
        .unwrap();
        let engine = ReferenceEngine::spawn(4);
        engine.try_open(path).unwrap();
        let opened = loop {
            if let Some(event) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break event;
            }
        };
        let ReferenceEngineEvent::Opened {
            revision: opened_revision,
            dictionary,
            ..
        } = opened
        else {
            panic!("reference did not open");
        };
        assert_eq!(dictionary.len(), 2);
        let clock = GenerationClock::default();
        let request = PanelGeneration::new(PanelId::new(7), ViewportGeneration::new(0));
        let interval = GenomicInterval::new(ContigId::new(0), 1, 5).unwrap();
        let query = ReferenceQueryIdentity::new(request, opened_revision, interval);
        engine.try_query(query, clock, "chr1".to_owned()).unwrap();
        let completed = loop {
            if let Some(event) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break event;
            }
        };
        let ReferenceEngineEvent::QueryCompleted {
            query: completed_query,
            slice,
            ..
        } = completed
        else {
            panic!("reference query did not complete");
        };
        assert_eq!(completed_query, query);
        assert_eq!(slice.interval, interval);
        assert_eq!(slice.bases, b"CGTA");
    }

    #[test]
    fn reference_revision_is_stable_within_an_open_and_changes_on_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("genome.fa");
        fs::write(&path, b">chr1\nACGTACGT\n").unwrap();
        fs::write(path.with_extension("fa.fai"), b"chr1\t8\t6\t8\t9\n").unwrap();
        let engine = ReferenceEngine::spawn(4);

        engine.try_open(path.clone()).unwrap();
        let first_open = loop {
            if let Some(event) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break event;
            }
        };
        let ReferenceEngineEvent::Opened {
            revision: first_revision,
            ..
        } = first_open
        else {
            panic!("first reference did not open");
        };

        let clock = GenerationClock::default();
        for panel in [11, 12] {
            let request = PanelGeneration::new(PanelId::new(panel), ViewportGeneration::new(0));
            let interval = GenomicInterval::new(ContigId::new(0), 0, 4).unwrap();
            let query = ReferenceQueryIdentity::new(request, first_revision, interval);
            engine
                .try_query(query, clock.clone(), "chr1".to_owned())
                .unwrap();
            let completed = loop {
                if let Some(event) = engine
                    .output
                    .receive_timeout(Duration::from_secs(2))
                    .unwrap()
                {
                    break event;
                }
            };
            let ReferenceEngineEvent::QueryCompleted {
                query: completed_query,
                ..
            } = completed
            else {
                panic!("reference query did not complete");
            };
            assert_eq!(completed_query, query);
        }

        engine.try_open(path).unwrap();
        let second_open = loop {
            if let Some(event) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break event;
            }
        };
        let ReferenceEngineEvent::Opened {
            revision: second_revision,
            ..
        } = second_open
        else {
            panic!("second reference did not open");
        };
        assert_ne!(second_revision, first_revision);

        let request = PanelGeneration::new(PanelId::new(13), ViewportGeneration::new(0));
        let interval = GenomicInterval::new(ContigId::new(0), 4, 8).unwrap();
        let query = ReferenceQueryIdentity::new(request, second_revision, interval);
        engine.try_query(query, clock, "chr1".to_owned()).unwrap();
        let completed = loop {
            if let Some(event) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break event;
            }
        };
        let ReferenceEngineEvent::QueryCompleted {
            query: completed_query,
            ..
        } = completed
        else {
            panic!("reopened reference query did not complete");
        };
        assert_eq!(completed_query, query);
    }

    #[test]
    fn query_for_an_obsolete_open_revision_is_silently_discarded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("genome.fa");
        fs::write(&path, b">chr1\nACGTACGT\n").unwrap();
        fs::write(path.with_extension("fa.fai"), b"chr1\t8\t6\t8\t9\n").unwrap();
        let engine = ReferenceEngine::spawn(4);

        engine.try_open(path.clone()).unwrap();
        let first_revision = loop {
            if let Some(ReferenceEngineEvent::Opened { revision, .. }) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break revision;
            }
        };
        engine.try_open(path).unwrap();
        let second_revision = loop {
            if let Some(ReferenceEngineEvent::Opened { revision, .. }) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break revision;
            }
        };
        assert_ne!(first_revision, second_revision);

        let interval = GenomicInterval::new(ContigId::new(0), 0, 4).unwrap();
        let stale = ReferenceQueryIdentity::new(
            PanelGeneration::new(PanelId::new(21), ViewportGeneration::new(0)),
            first_revision,
            interval,
        );
        let current = ReferenceQueryIdentity::new(
            PanelGeneration::new(PanelId::new(22), ViewportGeneration::new(0)),
            second_revision,
            interval,
        );
        let clock = GenerationClock::default();
        engine
            .try_query(stale, clock.clone(), "chr1".to_owned())
            .unwrap();
        engine.try_query(current, clock, "chr1".to_owned()).unwrap();

        let completed = loop {
            if let Some(event) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break event;
            }
        };
        assert!(matches!(
            completed,
            ReferenceEngineEvent::QueryCompleted { query, .. } if query == current
        ));
        assert_eq!(
            engine
                .output
                .receive_timeout(Duration::from_millis(100))
                .unwrap(),
            None
        );
    }

    #[test]
    fn same_generation_query_errors_retain_revision_and_exact_interval_identity() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("genome.fa");
        fs::write(&path, b">chr1\nACGTACGT\n").unwrap();
        fs::write(path.with_extension("fa.fai"), b"chr1\t8\t6\t8\t9\n").unwrap();
        let engine = ReferenceEngine::spawn(4);
        engine.try_open(path).unwrap();
        let revision = loop {
            if let Some(ReferenceEngineEvent::Opened { revision, .. }) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break revision;
            }
        };

        let request = PanelGeneration::new(PanelId::new(31), ViewportGeneration::new(0));
        let first = ReferenceQueryIdentity::new(
            request,
            revision,
            GenomicInterval::new(ContigId::new(0), 0, 9).unwrap(),
        );
        let second = ReferenceQueryIdentity::new(
            request,
            revision,
            GenomicInterval::new(ContigId::new(0), 1, 10).unwrap(),
        );
        let clock = GenerationClock::default();
        engine
            .try_query(first, clock.clone(), "chr1".to_owned())
            .unwrap();
        engine.try_query(second, clock, "chr1".to_owned()).unwrap();

        for expected in [first, second] {
            let event = loop {
                if let Some(event) = engine
                    .output
                    .receive_timeout(Duration::from_secs(2))
                    .unwrap()
                {
                    break event;
                }
            };
            assert!(matches!(
                event,
                ReferenceEngineEvent::Error {
                    query: Some(query),
                    ..
                } if query == expected
            ));
        }
    }

    #[test]
    fn logical_query_larger_than_one_fetch_chunk_completes_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("genome.fa");
        let sequence_len = usize::try_from(MAX_REFERENCE_QUERY_BASES + 2).unwrap();
        let mut fasta = Vec::with_capacity(sequence_len + 7);
        fasta.extend_from_slice(b">chr1\n");
        fasta.resize(fasta.len() + sequence_len, b'A');
        fasta.push(b'\n');
        fs::write(&path, fasta).unwrap();
        fs::write(
            path.with_extension("fa.fai"),
            format!(
                "chr1\t{sequence_len}\t6\t{sequence_len}\t{}\n",
                sequence_len + 1
            ),
        )
        .unwrap();
        let engine = ReferenceEngine::spawn(4);
        engine.try_open(path).unwrap();
        let revision = loop {
            if let Some(ReferenceEngineEvent::Opened { revision, .. }) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break revision;
            }
        };

        let interval =
            GenomicInterval::new(ContigId::new(0), 0, u64::try_from(sequence_len).unwrap())
                .unwrap();
        let query = ReferenceQueryIdentity::new(
            PanelGeneration::new(PanelId::new(32), ViewportGeneration::new(0)),
            revision,
            interval,
        );
        engine
            .try_query(query, GenerationClock::default(), "chr1".to_owned())
            .unwrap();

        let event = loop {
            if let Some(event) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break event;
            }
        };
        let ReferenceEngineEvent::QueryCompleted {
            query: completed_query,
            slice,
            ..
        } = event
        else {
            panic!("chunked logical reference query did not complete");
        };
        assert_eq!(completed_query, query);
        assert_eq!(slice.interval, interval);
        assert_eq!(slice.bases.len(), sequence_len);
        assert!(slice.bases.iter().all(|base| *base == b'A'));
    }

    #[test]
    fn open_errors_remain_identity_less_while_query_errors_are_identified() {
        let directory = tempfile::tempdir().unwrap();
        let engine = ReferenceEngine::spawn(4);
        engine
            .try_open(directory.path().join("missing.fa"))
            .unwrap();
        let open_error = loop {
            if let Some(event) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break event;
            }
        };
        assert!(matches!(
            open_error,
            ReferenceEngineEvent::Error { query: None, .. }
        ));

        let query = ReferenceQueryIdentity::new(
            PanelGeneration::new(PanelId::new(41), ViewportGeneration::new(0)),
            ReferenceRevision::new(RevisionFingerprint::new(9, 10)),
            GenomicInterval::new(ContigId::new(0), 0, 1).unwrap(),
        );
        engine
            .try_query(query, GenerationClock::default(), "chr1".to_owned())
            .unwrap();
        let query_error = loop {
            if let Some(event) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break event;
            }
        };
        assert!(matches!(
            query_error,
            ReferenceEngineEvent::Error {
                query: Some(identity),
                ..
            } if identity == query
        ));
    }
}
