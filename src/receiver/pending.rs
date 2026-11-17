//! Runtime-free publication, replay and delivery of coalesced receiver events.

use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;

use super::{LatestReport, PendingStatus, ReceiverEvent, ReceiverReport, ReceiverStatus};
use crate::{capture::Notification, Result};

/// The notification's kind and, for an unknown notification, its native value.
#[derive(Clone, Copy)]
enum EventKind<T = u32> {
    Status,
    Source,
    Unknown(T),
}

impl<T> EventKind<T> {
    /// The exhaustive successor rule owns delivery priority. Unit payloads
    /// select slots without inventing an unknown notification value.
    fn next(self) -> Option<EventKind<()>> {
        match self {
            Self::Status => Some(EventKind::Source),
            Self::Source => Some(EventKind::Unknown(())),
            Self::Unknown(_) => None,
        }
    }
}

impl From<&ReceiverEvent> for EventKind {
    fn from(event: &ReceiverEvent) -> Self {
        match *event {
            ReceiverEvent::StatusChanged(_) => Self::Status,
            ReceiverEvent::SourceChanged => Self::Source,
            ReceiverEvent::Unknown(value) => Self::Unknown(value),
        }
    }
}

impl From<Notification> for Option<EventKind> {
    fn from(notification: Notification) -> Self {
        match notification {
            Notification::None => None,
            Notification::StatusChanged => Some(EventKind::Status),
            Notification::SourceChanged => Some(EventKind::Source),
            Notification::Unknown(value) => Some(EventKind::Unknown(value)),
        }
    }
}

#[derive(Default)]
pub(crate) struct PendingEvents {
    // Exposed only to the receiver's existing status-protocol unit tests.
    pub(super) status: PendingStatus,
    source: AtomicBool,
    unknown: Mutex<LatestReport<u32>>,
}

impl PendingEvents {
    pub(super) fn publish(&self, notification: Notification) {
        if let Some(kind) = Option::<EventKind>::from(notification) {
            self.retain(kind, None);
        }
    }

    #[cfg(any(test, feature = "tokio", feature = "smol"))]
    pub(crate) fn replay(&self, event: ReceiverReport<ReceiverEvent>) {
        self.retain(EventKind::from(&event.value), Some(event.sequence));
    }

    /// Publication and replay share this one routing to the retained slots.
    fn retain(&self, kind: EventKind, sequence: Option<u64>) {
        match kind {
            EventKind::Status => self.status.publish(),
            EventKind::Source => self.source.store(true, Ordering::Release),
            EventKind::Unknown(value) => {
                let mut unknown = self.unknown.lock();
                match sequence {
                    #[cfg(any(test, feature = "tokio", feature = "smol"))]
                    Some(sequence) => unknown.replay(ReceiverReport { sequence, value }),
                    _ => unknown.publish(value),
                }
            }
        }
    }

    pub(crate) fn take(
        &self,
        snapshot: impl FnOnce() -> Result<ReceiverStatus>,
    ) -> Result<Option<ReceiverReport<ReceiverEvent>>> {
        if let Some(event) = self.status.take_or_else_with(
            || {
                snapshot()
                    .map(ReceiverEvent::StatusChanged)
                    .map(ReceiverReport::unsequenced)
            },
            || self.take_non_status(),
        )? {
            return Ok(Some(event));
        }
        Ok(self.take_non_status())
    }

    fn take_non_status(&self) -> Option<ReceiverReport<ReceiverEvent>> {
        let mut next = EventKind::<()>::Status.next();
        while let Some(kind) = next {
            // Both retention and taking must handle every kind. Together with
            // `next`, these exhaustive matches prevent an undeliverable slot.
            let event = match kind {
                EventKind::Status => unreachable!("status is taken before its successors"),
                EventKind::Source => self
                    .source
                    .swap(false, Ordering::AcqRel)
                    .then(|| ReceiverReport::unsequenced(ReceiverEvent::SourceChanged)),
                EventKind::Unknown(_) => self
                    .unknown
                    .lock()
                    .pending
                    .take()
                    .map(|report| report.map(ReceiverEvent::Unknown)),
            };
            if event.is_some() {
                return event;
            }
            next = kind.next();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;

    fn snapshot() -> Result<ReceiverStatus> {
        Ok(ReceiverStatus { connections: 1 })
    }

    fn take(pending: &PendingEvents) -> Option<ReceiverEvent> {
        pending
            .take(snapshot)
            .unwrap()
            .map(ReceiverReport::into_value)
    }

    #[test]
    fn media_source_and_unknown_events_are_retained_for_next_event() {
        let pending = PendingEvents::default();
        for _ in 0..3 {
            pending.publish(Notification::SourceChanged);
        }
        pending.publish(Notification::classify(987).unwrap());
        assert_eq!(take(&pending), Some(ReceiverEvent::SourceChanged));
        assert_eq!(take(&pending), Some(ReceiverEvent::Unknown(987)));
        assert_eq!(take(&pending), None);
        pending.publish(Notification::Unknown(988));
        pending.publish(Notification::SourceChanged);
        assert_eq!(take(&pending), Some(ReceiverEvent::SourceChanged));
        assert_eq!(take(&pending), Some(ReceiverEvent::Unknown(988)));
        assert_eq!(take(&pending), None);
    }

    #[test]
    fn media_status_is_retained_coalesced_and_consumed_once() {
        let pending = PendingEvents::default();
        let (_, allocations) = crate::test_alloc::allocation_count(|| {
            for _ in 0..3 {
                pending.publish(Notification::StatusChanged);
            }
            pending.publish(Notification::None);
            assert_eq!(
                take(&pending),
                Some(ReceiverEvent::StatusChanged(snapshot().unwrap()))
            );
            assert_eq!(take(&pending), None);
        });
        assert_eq!(allocations, 0);
    }

    #[test]
    fn combined_native_notifications_coalesce_with_events_retained_during_the_wait() {
        let pending = PendingEvents::default();
        let deliver = |notification| {
            pending.publish(notification);
            take(&pending)
        };
        pending.publish(Notification::SourceChanged);
        assert_eq!(
            deliver(Notification::SourceChanged),
            Some(ReceiverEvent::SourceChanged)
        );
        assert_eq!(take(&pending), None, "same invalidation must not replay");
        pending.publish(Notification::Unknown(987));
        assert_eq!(
            deliver(Notification::None),
            Some(ReceiverEvent::Unknown(987))
        );
        assert_eq!(take(&pending), None);
    }

    #[test]
    fn mixed_native_misses_deliver_concurrently_retained_events_once_without_allocating() {
        let pending = PendingEvents::default();
        let deliver = |notification| {
            pending.publish(notification);
            take(&pending)
        };
        assert_eq!(deliver(Notification::None), None);
        assert_eq!(
            deliver(Notification::Unknown(987)),
            Some(ReceiverEvent::Unknown(987))
        );
        assert_eq!(deliver(Notification::None), None);
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    pending.publish(Notification::SourceChanged);
                    pending.publish(Notification::Unknown(988));
                })
                .join()
                .unwrap();
        });
        let (events, allocations) = crate::test_alloc::allocation_count(|| {
            [
                deliver(Notification::None),
                deliver(Notification::None),
                deliver(Notification::None),
            ]
        });
        assert_eq!(allocations, 0);
        assert_eq!(
            events,
            [
                Some(ReceiverEvent::SourceChanged),
                Some(ReceiverEvent::Unknown(988)),
                None
            ]
        );
    }

    #[test]
    fn status_publication_is_consumed_once_and_newer_publication_survives() {
        let pending = PendingEvents::default();
        pending.publish(Notification::StatusChanged);
        assert!(take(&pending).is_some());
        assert_eq!(take(&pending), None);
        pending.publish(Notification::StatusChanged);
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let consumer = scope.spawn(|| {
                pending
                    .take(|| {
                        barrier.wait();
                        barrier.wait();
                        snapshot()
                    })
                    .unwrap()
            });
            barrier.wait();
            pending.publish(Notification::StatusChanged);
            barrier.wait();
            assert!(consumer.join().unwrap().is_none());
        });
        assert_eq!(
            take(&pending),
            Some(ReceiverEvent::StatusChanged(snapshot().unwrap()))
        );
        assert_eq!(take(&pending), None);
    }

    #[test]
    fn delivery_priority_is_status_then_source_then_unknown() {
        let pending = PendingEvents::default();
        pending.publish(Notification::Unknown(1));
        pending.publish(Notification::SourceChanged);
        pending.publish(Notification::StatusChanged);
        assert_eq!(
            take(&pending),
            Some(ReceiverEvent::StatusChanged(snapshot().unwrap()))
        );
        assert_eq!(take(&pending), Some(ReceiverEvent::SourceChanged));
        assert_eq!(take(&pending), Some(ReceiverEvent::Unknown(1)));
        assert_eq!(take(&pending), None);
    }

    #[test]
    fn failed_status_defers_to_other_events_and_reports_once_until_recovery() {
        let pending = PendingEvents::default();
        pending.publish(Notification::StatusChanged);
        pending.publish(Notification::SourceChanged);
        pending.publish(Notification::Unknown(1));
        let failed = || pending.take(|| Err(Error::ConnectionLost));
        assert_eq!(
            failed().unwrap().unwrap().value,
            ReceiverEvent::SourceChanged
        );
        assert_eq!(failed().unwrap().unwrap().value, ReceiverEvent::Unknown(1));
        assert!(matches!(failed(), Err(Error::ConnectionLost)));
        assert!(failed().unwrap().is_none());
        assert_eq!(
            take(&pending),
            Some(ReceiverEvent::StatusChanged(snapshot().unwrap()))
        );
        assert_eq!(take(&pending), None);
        pending.publish(Notification::StatusChanged);
        assert!(matches!(failed(), Err(Error::ConnectionLost)));
    }

    #[test]
    fn replay_cannot_replace_a_newer_unknown_publication() {
        let pending = PendingEvents::default();
        pending.publish(Notification::Unknown(1));
        let old = pending.take(snapshot).unwrap().unwrap();
        pending.publish(Notification::Unknown(2));
        pending.replay(old);
        assert_eq!(take(&pending), Some(ReceiverEvent::Unknown(2)));
        assert_eq!(take(&pending), None);
        pending.publish(Notification::Unknown(3));
        let current = pending.take(snapshot).unwrap().unwrap();
        pending.replay(current);
        assert_eq!(take(&pending), Some(ReceiverEvent::Unknown(3)));
        assert_eq!(take(&pending), None);
    }

    #[test]
    fn replay_uses_the_same_kind_routing_as_publication() {
        let pending = PendingEvents::default();
        pending.replay(ReceiverReport::unsequenced(ReceiverEvent::SourceChanged));
        pending.replay(ReceiverReport::unsequenced(ReceiverEvent::StatusChanged(
            ReceiverStatus::default(),
        )));
        assert_eq!(
            take(&pending),
            Some(ReceiverEvent::StatusChanged(snapshot().unwrap()))
        );
        assert_eq!(take(&pending), Some(ReceiverEvent::SourceChanged));
        assert_eq!(take(&pending), None);
    }

    #[test]
    fn concurrent_publishers_and_takers_consume_each_kind_once() {
        use std::sync::atomic::AtomicUsize;

        let pending = PendingEvents::default();
        let delivered = std::array::from_fn::<_, 3, _>(|_| AtomicUsize::new(0));
        let rounds = if cfg!(miri) { 3 } else { 64 };
        std::thread::scope(|scope| {
            for (index, notification) in [
                Notification::StatusChanged,
                Notification::SourceChanged,
                Notification::Unknown(1),
            ]
            .into_iter()
            .enumerate()
            {
                let pending = &pending;
                let delivered = &delivered;
                scope.spawn(move || {
                    for expected in 1..=rounds {
                        pending.publish(notification);
                        // Acknowledge each kind before publishing it again:
                        // these invalidations coalesce, rather than queue.
                        while delivered[index].load(Ordering::Acquire) < expected {
                            std::thread::yield_now();
                        }
                    }
                });
            }
            for _ in 0..2 {
                scope.spawn(|| loop {
                    if let Some(event) = take(&pending) {
                        let index = match event {
                            ReceiverEvent::StatusChanged(_) => 0,
                            ReceiverEvent::SourceChanged => 1,
                            ReceiverEvent::Unknown(_) => 2,
                        };
                        assert!(delivered[index].fetch_add(1, Ordering::AcqRel) < rounds);
                    } else if delivered
                        .iter()
                        .all(|count| count.load(Ordering::Acquire) == rounds)
                    {
                        break;
                    } else {
                        std::thread::yield_now();
                    }
                });
            }
        });
        assert_eq!(delivered.map(|count| count.into_inner()), [rounds; 3]);
        assert_eq!(take(&pending), None);
    }
}
