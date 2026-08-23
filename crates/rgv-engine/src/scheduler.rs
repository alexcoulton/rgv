use std::{
    num::NonZeroUsize,
    sync::Arc,
    thread::{self, JoinHandle},
};

use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError, bounded};
use rgv_core::{GenerationClock, ViewportGeneration};
use thiserror::Error;

#[derive(Debug)]
pub struct WorkItem<T> {
    pub generation: ViewportGeneration,
    pub payload: T,
}

#[derive(Debug)]
pub struct CompletedWork<T> {
    pub generation: ViewportGeneration,
    pub output: T,
}

/// A bounded worker pool that rejects stale work before and after processing.
pub struct Scheduler<I, O> {
    input: Option<Sender<WorkItem<I>>>,
    output: Option<Receiver<CompletedWork<O>>>,
    workers: Vec<JoinHandle<()>>,
}

impl<I, O> Scheduler<I, O>
where
    I: Send + 'static,
    O: Send + 'static,
{
    pub fn new<F>(
        worker_count: NonZeroUsize,
        queue_capacity: NonZeroUsize,
        clock: &GenerationClock,
        process: F,
    ) -> Self
    where
        F: Fn(I, &GenerationClock, ViewportGeneration) -> Option<O> + Send + Sync + 'static,
    {
        let (input_tx, input_rx) = bounded::<WorkItem<I>>(queue_capacity.get());
        let (output_tx, output_rx) = bounded::<CompletedWork<O>>(queue_capacity.get());
        let process = Arc::new(process);

        let workers = (0..worker_count.get())
            .map(|index| {
                let input = input_rx.clone();
                let output = output_tx.clone();
                let process = Arc::clone(&process);
                let clock = clock.clone();
                thread::Builder::new()
                    .name(format!("rgv-worker-{index}"))
                    .spawn(move || worker_loop(&input, &output, &clock, &process))
                    .expect("failed to spawn RGV worker")
            })
            .collect();

        Self {
            input: Some(input_tx),
            output: Some(output_rx),
            workers,
        }
    }

    pub fn try_submit(&self, work: WorkItem<I>) -> Result<(), SchedulerError<WorkItem<I>>> {
        let Some(input) = &self.input else {
            return Err(SchedulerError::Disconnected(work));
        };
        input.try_send(work).map_err(|error| match error {
            TrySendError::Full(work) => SchedulerError::QueueFull(work),
            TrySendError::Disconnected(work) => SchedulerError::Disconnected(work),
        })
    }

    pub fn try_receive(&self) -> Result<Option<CompletedWork<O>>, SchedulerError<()>> {
        let Some(output) = &self.output else {
            return Err(SchedulerError::Disconnected(()));
        };
        match output.try_recv() {
            Ok(work) => Ok(Some(work)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(SchedulerError::Disconnected(())),
        }
    }
}

impl<I, O> Drop for Scheduler<I, O> {
    fn drop(&mut self) {
        self.input.take();
        self.output.take();
        for worker in self.workers.drain(..) {
            if worker.join().is_err() {
                tracing::warn!("RGV worker panicked while shutting down");
            }
        }
    }
}

fn worker_loop<I, O, F>(
    input: &Receiver<WorkItem<I>>,
    output: &Sender<CompletedWork<O>>,
    clock: &GenerationClock,
    process: &Arc<F>,
) where
    I: Send + 'static,
    O: Send + 'static,
    F: Fn(I, &GenerationClock, ViewportGeneration) -> Option<O> + Send + Sync + 'static,
{
    while let Ok(work) = input.recv() {
        if !clock.is_current(work.generation) {
            continue;
        }
        let generation = work.generation;
        if let Some(result) = process(work.payload, clock, generation)
            && clock.is_current(generation)
            && output
                .send(CompletedWork {
                    generation,
                    output: result,
                })
                .is_err()
        {
            break;
        }
    }
}

#[derive(Debug, Error)]
pub enum SchedulerError<T> {
    #[error("the bounded work queue is full")]
    QueueFull(T),
    #[error("the work queue is disconnected")]
    Disconnected(T),
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroUsize, thread, time::Duration};

    use super::*;

    #[test]
    fn stale_work_never_reaches_results() {
        let clock = GenerationClock::default();
        let scheduler = Scheduler::new(
            NonZeroUsize::new(1).unwrap(),
            NonZeroUsize::new(4).unwrap(),
            &clock,
            |value: u32, _, _| {
                thread::sleep(Duration::from_millis(5));
                Some(value * 2)
            },
        );

        let old = clock.current();
        scheduler
            .try_submit(WorkItem {
                generation: old,
                payload: 21,
            })
            .unwrap();
        clock.advance();
        thread::sleep(Duration::from_millis(15));
        assert!(scheduler.try_receive().unwrap().is_none());
    }

    #[test]
    fn current_work_is_delivered() {
        let clock = GenerationClock::default();
        let scheduler = Scheduler::new(
            NonZeroUsize::new(1).unwrap(),
            NonZeroUsize::new(2).unwrap(),
            &clock,
            |value: u32, _, _| Some(value * 2),
        );
        scheduler
            .try_submit(WorkItem {
                generation: clock.current(),
                payload: 21,
            })
            .unwrap();

        let result = (0..100).find_map(|_| {
            let result = scheduler.try_receive().unwrap();
            if result.is_none() {
                thread::yield_now();
            }
            result
        });
        assert_eq!(result.unwrap().output, 42);
    }

    #[test]
    fn shutdown_unblocks_worker_when_result_queue_is_full() {
        let clock = GenerationClock::default();
        let (processed_tx, processed_rx) = crossbeam_channel::bounded(2);
        let scheduler = Scheduler::new(
            NonZeroUsize::new(1).unwrap(),
            NonZeroUsize::new(2).unwrap(),
            &clock,
            move |value: u32, _, _| {
                processed_tx.send(()).unwrap();
                Some(value)
            },
        );
        for value in 1..=2 {
            scheduler
                .try_submit(WorkItem {
                    generation: clock.current(),
                    payload: value,
                })
                .unwrap();
        }
        processed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        processed_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        drop(scheduler);
    }
}
