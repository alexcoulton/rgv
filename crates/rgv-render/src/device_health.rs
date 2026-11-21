//! Records GPU device loss and uncaptured wgpu errors instead of letting them abort the process.
//!
//! wgpu's default uncaptured-error handler panics, and nothing observes a lost device unless a
//! callback is installed. [`GpuDeviceHealth::watch`] installs both handlers on a device; the
//! application polls [`GpuDeviceHealth::is_lost`] once per frame and, on loss, recreates the
//! device and its renderers (see `AlignmentGpuRenderer::rebuild_for_device`).
use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

/// Why a watched device stopped being usable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GpuDeviceLoss {
    /// `true` when the application destroyed the device itself.
    pub destroyed_by_application: bool,
    pub message: String,
}

/// Shared, thread-safe health record for the currently watched device.
///
/// One record outlives any number of devices: each [`Self::watch`] call starts a new epoch and
/// clears the loss, and a late callback from a previously watched device is ignored.
#[derive(Debug, Default)]
pub struct GpuDeviceHealth {
    epoch: AtomicU64,
    lost: AtomicBool,
    loss: Mutex<Option<GpuDeviceLoss>>,
    uncaptured_errors: AtomicU64,
    last_error: Mutex<Option<String>>,
}

impl GpuDeviceHealth {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Installs the device-lost callback and the uncaptured-error handler on `device` and makes
    /// it the device this record describes. Any loss recorded for an earlier device is cleared.
    pub fn watch(self: &Arc<Self>, device: &wgpu::Device) {
        let epoch = self.begin_epoch();
        let health = Arc::clone(self);
        device.set_device_lost_callback(move |reason, message| {
            health.record_loss(
                epoch,
                matches!(reason, wgpu::DeviceLostReason::Destroyed),
                message,
            );
        });
        let health = Arc::clone(self);
        device.on_uncaptured_error(Arc::new(move |error: wgpu::Error| {
            health.record_uncaptured_error(epoch, error.to_string());
        }));
    }

    /// Starts describing a new device and returns the epoch its callbacks must report under.
    pub fn begin_epoch(&self) -> u64 {
        let epoch = self.epoch.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        *self.loss.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.lost.store(false, Ordering::SeqCst);
        epoch
    }

    /// Records that the device watched under `epoch` was lost. Returns `false`, changing
    /// nothing, when a newer device has been watched since.
    pub fn record_loss(
        &self,
        epoch: u64,
        destroyed_by_application: bool,
        message: impl Into<String>,
    ) -> bool {
        let mut loss = self.loss.lock().unwrap_or_else(PoisonError::into_inner);
        if epoch != self.epoch.load(Ordering::SeqCst) {
            return false;
        }
        // Keep the first report: it names the cause, later ones only its consequences.
        loss.get_or_insert_with(|| GpuDeviceLoss {
            destroyed_by_application,
            message: message.into(),
        });
        self.lost.store(true, Ordering::SeqCst);
        true
    }

    /// Records an error no error scope captured. Returns `false` for a superseded device.
    pub fn record_uncaptured_error(&self, epoch: u64, message: impl Into<String>) -> bool {
        if epoch != self.epoch.load(Ordering::SeqCst) {
            return false;
        }
        self.uncaptured_errors.fetch_add(1, Ordering::SeqCst);
        *self
            .last_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(message.into());
        true
    }

    /// Whether the currently watched device has been lost. Cheap enough to poll every frame.
    #[must_use]
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }

    /// The loss recorded for the currently watched device, if any.
    #[must_use]
    pub fn loss(&self) -> Option<GpuDeviceLoss> {
        self.loss
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Number of uncaptured errors reported by the watched devices so far.
    #[must_use]
    pub fn uncaptured_error_count(&self) -> u64 {
        self.uncaptured_errors.load(Ordering::SeqCst)
    }

    /// Removes and returns the most recent uncaptured error, so each is reported once.
    #[must_use]
    pub fn take_last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_loss_is_recorded_once_and_cleared_by_the_next_device() {
        let health = GpuDeviceHealth::new();
        assert!(!health.is_lost());
        assert_eq!(health.loss(), None);

        let first = health.begin_epoch();
        assert!(health.record_loss(first, false, "driver reset"));
        assert!(health.record_loss(first, false, "follow-on failure"));
        assert!(health.is_lost());
        assert_eq!(
            health.loss(),
            Some(GpuDeviceLoss {
                destroyed_by_application: false,
                message: "driver reset".to_owned(),
            })
        );

        // Recovery watches a replacement device; the old device's late callbacks are ignored.
        let second = health.begin_epoch();
        assert!(!health.is_lost());
        assert_eq!(health.loss(), None);
        assert!(!health.record_loss(first, true, "old device dropped"));
        assert!(!health.record_uncaptured_error(first, "old device error"));
        assert!(!health.is_lost());
        assert_eq!(health.uncaptured_error_count(), 0);

        assert!(health.record_loss(second, true, "destroyed"));
        assert!(health.loss().unwrap().destroyed_by_application);
    }

    #[test]
    fn uncaptured_errors_are_counted_and_reported_once() {
        let health = GpuDeviceHealth::new();
        let epoch = health.begin_epoch();
        assert!(health.record_uncaptured_error(epoch, "validation error A"));
        assert!(health.record_uncaptured_error(epoch, "validation error B"));
        assert_eq!(health.uncaptured_error_count(), 2);
        assert_eq!(
            health.take_last_error().as_deref(),
            Some("validation error B")
        );
        assert_eq!(health.take_last_error(), None);
        // An uncaptured error alone does not mark the device lost.
        assert!(!health.is_lost());
    }
}
