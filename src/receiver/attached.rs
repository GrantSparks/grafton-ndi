//! Ownership shared by receiver-attached native synchronizers.

use std::{ops::Deref, sync::Arc};

use super::{RawCapture, Receiver, ReceiverCore, SynchronizerMode};
use crate::sys::bindings::NDIlib_recv_instance_t;
use crate::{Rejected, Result};

pub(crate) mod sealed {
    pub trait Sealed {}
}

/// A native synchronizer and the receiver mode and capture view it supplies.
///
/// # Safety
///
/// `Mode` must reserve every receiver capture kind this synchronizer owns.
/// `destroy` must use the creating runtime. `capture` must borrow only the
/// resource it receives; neither it nor the returned view may replace the
/// resource or restore raw receiver capture.
pub(crate) unsafe trait Synchronizer: sealed::Sealed + Sized + 'static {
    type Mode: SynchronizerMode;
    type Capture<'a>;

    /// Destroy this synchronizer through its creating receiver's runtime.
    ///
    /// # Safety
    ///
    /// The handle must be live, uniquely owned and created from `receiver`.
    /// It may be destroyed only once and no captured frame may remain live.
    unsafe fn destroy(&mut self, receiver: &ReceiverCore);
    fn capture(resource: &Resource<Self>) -> Self::Capture<'_>;
}

/// A synchronizer keeps its creating receiver and runtime through destruction.
/// Construction is private to `Attached::adopt`.
pub(crate) struct Resource<S: Synchronizer> {
    native: S,
    pub(crate) receiver: Arc<ReceiverCore>,
}

impl<S: Synchronizer> Deref for Resource<S> {
    type Target = S;

    fn deref(&self) -> &S {
        &self.native
    }
}

impl<S: Synchronizer> Drop for Resource<S> {
    fn drop(&mut self) {
        // SAFETY: only adoption constructs this resource, pairing its unique
        // handle with this retained receiver. Views and frame guards borrow
        // the resource, so they have ended before its one destruction.
        unsafe { self.native.destroy(&self.receiver) };
    }
}

/// Borrow of the by-value receiver being adopted. Only `Attached::adopt`
/// constructs it, after reserving that receiver's raw capture mode.
pub(crate) struct Adoption<'a> {
    core: &'a ReceiverCore,
    _exclusive: parking_lot::MutexGuard<'a, super::ConnectionPermit>,
}

impl Adoption<'_> {
    pub(crate) fn core(&self) -> &ReceiverCore {
        self.core
    }

    pub(crate) fn instance(&self) -> NDIlib_recv_instance_t {
        self.core.instance.0
    }
}

/// The synchronizer and receiver can be paired only by adoption and recovered
/// only together. No mutable receiver/resource fields escape this owner.
pub(crate) struct Attached<S: Synchronizer> {
    resource: Resource<S>,
    receiver: Receiver<S::Mode>,
}

impl<S: Synchronizer> Attached<S> {
    /// Adopt a new native synchronizer created from this receiver.
    ///
    /// # Safety
    ///
    /// `create` must return a uniquely owned live synchronizer created from
    /// the supplied receiver through its runtime, or fail without retaining
    /// any synchronizer. Its `Synchronizer` implementation must describe that
    /// native object. The receiver is returned unchanged on failure.
    pub(crate) unsafe fn adopt(
        receiver: Receiver,
        create: impl FnOnce(Adoption<'_>) -> Result<S>,
    ) -> std::result::Result<Self, Rejected<Receiver>> {
        let adoption = Adoption {
            core: &receiver.core,
            _exclusive: receiver.core.connection_guard.lock(),
        };
        let native = match create(adoption) {
            Ok(native) => native,
            Err(error) => return Err(Rejected::new(error, receiver)),
        };
        Ok(Self {
            resource: Resource {
                native,
                receiver: Arc::clone(&receiver.core),
            },
            receiver: Receiver {
                core: receiver.core,
                _mode: std::marker::PhantomData,
            },
        })
    }

    pub(crate) fn receiver(&self) -> &Receiver<S::Mode> {
        &self.receiver
    }

    #[cfg(all(test, feature = "advanced"))]
    pub(crate) fn resource(&self) -> &Resource<S> {
        &self.resource
    }

    pub(crate) fn capture(&self) -> S::Capture<'_> {
        S::capture(&self.resource)
    }

    #[cfg(feature = "advanced")]
    pub(crate) fn controls(&mut self) -> crate::advanced::ReceiverControls<'_, S::Mode> {
        crate::advanced::ReceiverControls::new(&mut self.receiver)
    }

    #[cfg(feature = "advanced")]
    pub(crate) fn split(
        &mut self,
    ) -> (
        S::Capture<'_>,
        crate::advanced::ReceiverControls<'_, S::Mode>,
    ) {
        (
            S::capture(&self.resource),
            crate::advanced::ReceiverControls::new(&mut self.receiver),
        )
    }

    pub(crate) fn into_receiver(self) -> Receiver<RawCapture> {
        let Self { resource, receiver } = self;
        drop(resource);
        Receiver {
            core: receiver.core,
            _mode: std::marker::PhantomData,
        }
    }
}
