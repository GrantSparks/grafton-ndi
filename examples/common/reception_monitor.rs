//! Example-local recent-reception heuristic for one expected media stream.
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reception {
    Disconnected,
    AwaitingInput,
    Recent,
    Starved,
}

/// Poll on a diagnostics path. The caller chooses the appropriate received
/// counter (audio, video or metadata) and a positive cadence-dependent grace.
pub struct ReceptionMonitor {
    grace: Duration,
    baseline: Option<u64>,
    connected: bool,
    last_progress: Option<Instant>,
}

impl ReceptionMonitor {
    pub fn new(grace: Duration) -> Self {
        assert!(!grace.is_zero(), "the grace period must be positive");
        Self {
            grace,
            baseline: None,
            connected: false,
            last_progress: None,
        }
    }

    /// Call after an explicit source/receiver change, connect, disconnect or
    /// reconnect. A later capture or old format cache cannot establish recovery.
    pub fn reset(&mut self) {
        self.baseline = None;
        self.connected = false;
        self.last_progress = None;
    }

    /// Observe native connection state and the selected received-frame total.
    /// A positive first total establishes only a baseline, not recent input.
    pub fn observe(&mut self, connected: bool, total: u64, now: Instant) -> Reception {
        if !connected {
            self.reset();
            return Reception::Disconnected;
        }
        if !self.connected {
            self.connected = true;
            self.baseline = Some(total);
            return Reception::AwaitingInput;
        }
        let previous = self
            .baseline
            .replace(total)
            .expect("connected has baseline");
        match total.checked_sub(previous) {
            None => {
                self.reset();
                self.connected = true;
                self.baseline = Some(total);
                Reception::AwaitingInput
            }
            Some(0) => match self.last_progress {
                None => Reception::AwaitingInput,
                Some(last) if now.saturating_duration_since(last) >= self.grace => {
                    Reception::Starved
                }
                Some(_) => Reception::Recent,
            },
            Some(_) => {
                self.last_progress = Some(now);
                Reception::Recent
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_silent_progress_grace_and_recovery() {
        let start = Instant::now();
        let mut monitor = ReceptionMonitor::new(Duration::from_secs(2));
        assert_eq!(monitor.observe(true, 100, start), Reception::AwaitingInput);
        assert_eq!(
            monitor.observe(true, 100, start + Duration::from_secs(10)),
            Reception::AwaitingInput
        );
        // Zero-valued source samples still arrive as packets and advance this
        // audio counter. Amplitude is not an input to the monitor.
        assert_eq!(
            monitor.observe(true, 101, start + Duration::from_secs(11)),
            Reception::Recent
        );
        assert_eq!(
            monitor.observe(true, 101, start + Duration::from_secs(12)),
            Reception::Recent
        );
        assert_eq!(
            monitor.observe(true, 101, start + Duration::from_secs(13)),
            Reception::Starved
        );
        assert_eq!(
            monitor.observe(true, 102, start + Duration::from_secs(14)),
            Reception::Recent
        );
    }

    #[test]
    fn disconnect_decrease_and_explicit_replacement_clear_history() {
        let start = Instant::now();
        let mut monitor = ReceptionMonitor::new(Duration::from_secs(1));
        assert_eq!(monitor.observe(true, 5, start), Reception::AwaitingInput);
        assert_eq!(monitor.observe(true, 6, start), Reception::Recent);
        assert_eq!(monitor.observe(false, 6, start), Reception::Disconnected);
        assert_eq!(monitor.observe(true, 6, start), Reception::AwaitingInput);
        assert_eq!(monitor.observe(true, 7, start), Reception::Recent);
        assert_eq!(monitor.observe(true, 0, start), Reception::AwaitingInput);
        assert_eq!(monitor.observe(true, 0, start), Reception::AwaitingInput);
        assert_eq!(monitor.observe(true, 1, start), Reception::Recent);
        monitor.reset(); // explicit source or receiver replacement
        assert_eq!(monitor.observe(true, 500, start), Reception::AwaitingInput);
        assert_eq!(monitor.observe(true, 501, start), Reception::Recent);
    }

    #[test]
    fn audio_only_uses_audio_progress_and_ignores_video_zero() {
        let start = Instant::now();
        let mut monitor = ReceptionMonitor::new(Duration::from_millis(500));
        let video_received = 0;
        let audio_received = [0, 1, 2];
        assert_eq!(video_received, 0);
        assert_eq!(
            monitor.observe(true, audio_received[0], start),
            Reception::AwaitingInput
        );
        assert_eq!(
            monitor.observe(true, audio_received[1], start),
            Reception::Recent
        );
        assert_eq!(
            monitor.observe(true, audio_received[2], start),
            Reception::Recent
        );
    }
}
