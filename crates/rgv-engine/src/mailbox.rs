use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

struct Stamped<T> {
    sequence: u128,
    value: T,
}

struct MailboxState<T> {
    controls: VecDeque<Stamped<T>>,
    latest: VecDeque<(u64, Stamped<T>)>,
    next_sequence: u128,
    sender_open: bool,
    receiver_open: bool,
}

struct MailboxInner<T> {
    control_capacity: usize,
    state: Mutex<MailboxState<T>>,
    has_data: Condvar,
    has_control_capacity: Condvar,
}

pub(crate) struct CoalescingSender<T> {
    inner: Arc<MailboxInner<T>>,
}

pub(crate) struct CoalescingReceiver<T> {
    inner: Arc<MailboxInner<T>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MailboxDisconnected;

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum TryMailboxSendError<T> {
    Full(T),
    Disconnected(T),
}

pub(crate) fn bounded_coalescing<T>(
    control_capacity: usize,
) -> (CoalescingSender<T>, CoalescingReceiver<T>) {
    let inner = Arc::new(MailboxInner {
        control_capacity: control_capacity.max(1),
        state: Mutex::new(MailboxState {
            controls: VecDeque::new(),
            latest: VecDeque::new(),
            next_sequence: 0,
            sender_open: true,
            receiver_open: true,
        }),
        has_data: Condvar::new(),
        has_control_capacity: Condvar::new(),
    });
    (
        CoalescingSender {
            inner: Arc::clone(&inner),
        },
        CoalescingReceiver { inner },
    )
}

impl<T> CoalescingSender<T> {
    /// Enqueues a non-discardable control without ever waiting for capacity. UI and render
    /// threads use this form so a saturated worker cannot stall input handling.
    pub(crate) fn try_send_control(&self, value: T) -> Result<(), TryMailboxSendError<T>> {
        let mut state = self.inner.state.lock().expect("mailbox mutex poisoned");
        if !state.receiver_open {
            return Err(TryMailboxSendError::Disconnected(value));
        }
        if state.controls.len() >= self.inner.control_capacity {
            return Err(TryMailboxSendError::Full(value));
        }
        let stamped = stamp(&mut state, value);
        state.controls.push_back(stamped);
        self.inner.has_data.notify_one();
        Ok(())
    }

    /// Enqueues an event that must not be discarded. This applies backpressure
    /// when the bounded control queue is full and unblocks if the receiver is
    /// closed during shutdown.
    pub(crate) fn send_control(&self, value: T) -> Result<(), T> {
        let mut state = self.inner.state.lock().expect("mailbox mutex poisoned");
        while state.controls.len() >= self.inner.control_capacity && state.receiver_open {
            state = self
                .inner
                .has_control_capacity
                .wait(state)
                .expect("mailbox mutex poisoned");
        }
        if !state.receiver_open {
            return Err(value);
        }
        let stamped = stamp(&mut state, value);
        state.controls.push_back(stamped);
        self.inner.has_data.notify_one();
        Ok(())
    }

    /// Publishes replaceable viewport data. At most one such event is retained,
    /// and a newer result atomically supersedes an older result the UI has not
    /// consumed yet.
    /// Publishes replaceable viewport data for one stable owner. A newer
    /// value supersedes only that owner's unread value; other panels retain
    /// their own latest result.
    pub(crate) fn send_latest_for(&self, owner: u64, value: T) -> Result<(), T> {
        let mut state = self.inner.state.lock().expect("mailbox mutex poisoned");
        if !state.receiver_open {
            return Err(value);
        }
        if let Some(index) = state
            .latest
            .iter()
            .position(|(candidate, _)| *candidate == owner)
        {
            state.latest.remove(index);
        }
        let stamped = stamp(&mut state, value);
        state.latest.push_back((owner, stamped));
        self.inner.has_data.notify_one();
        Ok(())
    }
}

impl<T> Drop for CoalescingSender<T> {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().expect("mailbox mutex poisoned");
        state.sender_open = false;
        self.inner.has_data.notify_all();
    }
}

impl<T> CoalescingReceiver<T> {
    pub(crate) fn try_receive(&self) -> Result<Option<T>, MailboxDisconnected> {
        let mut state = self.inner.state.lock().expect("mailbox mutex poisoned");
        if let Some(value) = take_next(&mut state) {
            self.inner.has_control_capacity.notify_one();
            return Ok(Some(value));
        }
        if state.sender_open {
            Ok(None)
        } else {
            Err(MailboxDisconnected)
        }
    }

    pub(crate) fn receive_timeout(
        &self,
        timeout: Duration,
    ) -> Result<Option<T>, MailboxDisconnected> {
        let started = Instant::now();
        let mut state = self.inner.state.lock().expect("mailbox mutex poisoned");
        loop {
            if let Some(value) = take_next(&mut state) {
                self.inner.has_control_capacity.notify_one();
                return Ok(Some(value));
            }
            if !state.sender_open {
                return Err(MailboxDisconnected);
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Ok(None);
            }
            let (next_state, wait) = self
                .inner
                .has_data
                .wait_timeout(state, remaining)
                .expect("mailbox mutex poisoned");
            state = next_state;
            if wait.timed_out() && state.controls.is_empty() && state.latest.is_empty() {
                return Ok(None);
            }
        }
    }

    /// Closes the consumer side before worker joins so a producer waiting on
    /// bounded control capacity can terminate cleanly.
    pub(crate) fn close(&self) {
        let mut state = self.inner.state.lock().expect("mailbox mutex poisoned");
        state.receiver_open = false;
        self.inner.has_control_capacity.notify_all();
    }
}

fn stamp<T>(state: &mut MailboxState<T>, value: T) -> Stamped<T> {
    let sequence = state.next_sequence;
    state.next_sequence = state
        .next_sequence
        .checked_add(1)
        .expect("mailbox sequence exhausted");
    Stamped { sequence, value }
}

fn take_next<T>(state: &mut MailboxState<T>) -> Option<T> {
    let control_is_next = match (state.controls.front(), state.latest.front()) {
        (Some(control), Some((_, latest))) => control.sequence < latest.sequence,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => return None,
    };
    if control_is_next {
        state.controls.pop_front().map(|event| event.value)
    } else {
        state.latest.pop_front().map(|(_, event)| event.value)
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn newest_replaceable_value_supersedes_an_unread_value() {
        let (sender, receiver) = bounded_coalescing(1);
        sender.send_latest_for(0, 1).unwrap();
        sender.send_latest_for(0, 2).unwrap();
        sender.send_latest_for(0, 3).unwrap();

        assert_eq!(receiver.try_receive().unwrap(), Some(3));
        assert_eq!(receiver.try_receive().unwrap(), None);
    }

    #[test]
    fn newest_replaceable_value_is_retained_per_owner() {
        let (sender, receiver) = bounded_coalescing(1);
        sender.send_latest_for(1, "panel 1 old").unwrap();
        sender.send_latest_for(2, "panel 2").unwrap();
        sender.send_latest_for(1, "panel 1 new").unwrap();

        assert_eq!(receiver.try_receive().unwrap(), Some("panel 2"));
        assert_eq!(receiver.try_receive().unwrap(), Some("panel 1 new"));
        assert_eq!(receiver.try_receive().unwrap(), None);
    }

    #[test]
    fn control_events_retain_order_around_latest_value() {
        let (sender, receiver) = bounded_coalescing(2);
        sender.send_control("ready").unwrap();
        sender.send_latest_for(0, "old viewport").unwrap();
        sender.send_control("warning").unwrap();
        sender.send_latest_for(0, "new viewport").unwrap();

        assert_eq!(receiver.try_receive().unwrap(), Some("ready"));
        assert_eq!(receiver.try_receive().unwrap(), Some("warning"));
        assert_eq!(receiver.try_receive().unwrap(), Some("new viewport"));
    }

    #[test]
    fn closing_receiver_unblocks_a_full_control_queue() {
        let (sender, receiver) = bounded_coalescing(1);
        sender.send_control(1).unwrap();
        let blocked = thread::spawn(move || sender.send_control(2));

        receiver.close();

        assert_eq!(blocked.join().unwrap(), Err(2));
    }

    #[test]
    fn nonblocking_control_reports_full_and_disconnected_without_dropping_order() {
        let (sender, receiver) = bounded_coalescing(1);
        sender.try_send_control(1).unwrap();
        assert_eq!(
            sender.try_send_control(2),
            Err(TryMailboxSendError::Full(2))
        );
        assert_eq!(receiver.try_receive().unwrap(), Some(1));
        receiver.close();
        assert_eq!(
            sender.try_send_control(3),
            Err(TryMailboxSendError::Disconnected(3))
        );
    }

    #[test]
    fn timeout_and_sender_disconnect_are_distinct() {
        let (sender, receiver) = bounded_coalescing::<u8>(1);
        assert_eq!(
            receiver.receive_timeout(Duration::from_millis(1)).unwrap(),
            None
        );
        drop(sender);
        assert_eq!(receiver.try_receive(), Err(MailboxDisconnected));
    }
}
