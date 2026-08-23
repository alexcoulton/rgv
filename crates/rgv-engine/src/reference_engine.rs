use std::{
    path::PathBuf,
    sync::Arc,
    thread::{self, JoinHandle},
};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use rgv_core::{GenerationClock, GenomicInterval, PanelGeneration, ReferenceDictionary};
use rgv_hts::{
    FastaOpenOptions, FastaReference, ReferenceSlice, RemoteCancellation, RemoteIoStats,
};

use crate::{
    EngineQueueError,
    mailbox::{CoalescingReceiver, CoalescingSender, bounded_coalescing},
};

pub const MAX_REFERENCE_QUERY_BASES: u64 = 1_000_000;

enum ReferenceEngineRequest {
    Open {
        path: PathBuf,
        options: FastaOpenOptions,
    },
    Query {
        request: PanelGeneration,
        clock: GenerationClock,
        contig_name: String,
        interval: GenomicInterval,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReferenceEngineEvent {
    Opened {
        dictionary: ReferenceDictionary,
        remote_io_stats: Option<RemoteIoStats>,
    },
    QueryCompleted {
        request: PanelGeneration,
        slice: Arc<ReferenceSlice>,
        remote_io_stats: Option<RemoteIoStats>,
    },
    Error {
        request: Option<PanelGeneration>,
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
        request: PanelGeneration,
        clock: GenerationClock,
        contig_name: String,
        interval: GenomicInterval,
    ) -> Result<(), EngineQueueError> {
        self.try_send(ReferenceEngineRequest::Query {
            request,
            clock,
            contig_name,
            interval,
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
    let mut reference = None;
    while let Ok(request) = input.recv() {
        let event = match request {
            ReferenceEngineRequest::Open { path, options } => {
                match FastaReference::open_with_options(path, &options) {
                    Ok(opened) => {
                        let dictionary = opened.reference_dictionary().clone();
                        let remote_io_stats = opened.remote_io_stats();
                        reference = Some(opened);
                        ReferenceEngineEvent::Opened {
                            dictionary,
                            remote_io_stats,
                        }
                    }
                    Err(error) => ReferenceEngineEvent::Error {
                        request: None,
                        message: error.to_string(),
                    },
                }
            }
            ReferenceEngineRequest::Query {
                request,
                clock,
                contig_name,
                interval,
            } => {
                let Some(reference) = reference.as_ref() else {
                    send_event(
                        output,
                        ReferenceEngineEvent::Error {
                            request: Some(request),
                            message: "reference FASTA is not open".to_owned(),
                        },
                    );
                    continue;
                };
                if interval.len() > MAX_REFERENCE_QUERY_BASES {
                    ReferenceEngineEvent::Error {
                        request: Some(request),
                        message: format!(
                            "reference query exceeds the {MAX_REFERENCE_QUERY_BASES}-base limit"
                        ),
                    }
                } else {
                    let query_clock = clock.clone();
                    let generation = request.generation();
                    let cancellation: RemoteCancellation =
                        Arc::new(move || query_clock.is_current(generation));
                    reference.set_remote_cancellation(Some(cancellation));
                    match reference.fetch_slice_by_name(&contig_name, interval) {
                        Ok(Some(slice)) if clock.is_current(request.generation()) => {
                            ReferenceEngineEvent::QueryCompleted {
                                request,
                                slice: Arc::new(slice),
                                remote_io_stats: reference.remote_io_stats(),
                            }
                        }
                        Ok(None) if clock.is_current(request.generation()) => {
                            ReferenceEngineEvent::Error {
                                request: Some(request),
                                message: format!(
                                    "reference FASTA does not contain contig {contig_name}"
                                ),
                            }
                        }
                        Err(error) if clock.is_current(request.generation()) => {
                            ReferenceEngineEvent::Error {
                                request: Some(request),
                                message: error.to_string(),
                            }
                        }
                        Ok(_) | Err(_) => continue,
                    }
                }
            }
        };
        send_event(output, event);
    }
}

fn send_event(output: &CoalescingSender<ReferenceEngineEvent>, event: ReferenceEngineEvent) {
    let result = if let ReferenceEngineEvent::QueryCompleted { request, .. } = &event {
        output.send_latest_for(request.panel_id().get(), event)
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
        let ReferenceEngineEvent::Opened { dictionary, .. } = opened else {
            panic!("reference did not open");
        };
        assert_eq!(dictionary.len(), 2);
        let clock = GenerationClock::default();
        let request = PanelGeneration::new(PanelId::new(7), ViewportGeneration::new(0));
        engine
            .try_query(
                request,
                clock,
                "chr1".to_owned(),
                GenomicInterval::new(ContigId::new(0), 1, 5).unwrap(),
            )
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
        let ReferenceEngineEvent::QueryCompleted { slice, .. } = completed else {
            panic!("reference query did not complete");
        };
        assert_eq!(slice.bases, b"CGTA");
    }
}
