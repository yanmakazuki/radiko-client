//! Numeric request timing only. No targets, headers, or response content are retained.
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Idle,
    Headers,
    Body,
    RetryWait,
}

#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    pub stage: Stage,
    /// Status received for the current/latest attempt, cleared before a new attempt.
    pub response_status: Option<u16>,
    pub elapsed: Duration,
    pub headers_max: Duration,
    pub body_max: Duration,
    pub attempts: u64,
    /// Backoffs entered, including a backoff cancelled before the next request.
    pub retry_waits: u64,
    /// reqwest failures only; HTTP status errors and cancellation are excluded.
    pub network_failures: u64,
    /// These subsets can overlap (for example a connection timeout).
    pub timeouts: u64,
    pub connect_failures: u64,
    /// Completed stream-token renewals. HTTP 401 retries that fail do not add another renewal.
    pub reauthentications: u64,
}

struct State {
    began: Instant,
    snapshot: Snapshot,
}
/// An optional observer shared by the acquisition worker and its owning controller.
pub struct Measurements(Mutex<State>);
impl Measurements {
    pub(crate) fn new() -> Self {
        Self(Mutex::new(State {
            began: Instant::now(),
            snapshot: Snapshot {
                stage: Stage::Idle,
                response_status: None,
                elapsed: Duration::ZERO,
                headers_max: Duration::ZERO,
                body_max: Duration::ZERO,
                attempts: 0,
                retry_waits: 0,
                network_failures: 0,
                timeouts: 0,
                connect_failures: 0,
                reauthentications: 0,
            },
        }))
    }
    pub(crate) fn enter(&self, stage: Stage) {
        self.enter_at(stage, Instant::now());
    }
    fn enter_at(&self, stage: Stage, now: Instant) {
        let Ok(mut state) = self.0.lock() else {
            return;
        };
        let elapsed = now.saturating_duration_since(state.began);
        match state.snapshot.stage {
            Stage::Headers => state.snapshot.headers_max = state.snapshot.headers_max.max(elapsed),
            Stage::Body => state.snapshot.body_max = state.snapshot.body_max.max(elapsed),
            Stage::Idle | Stage::RetryWait => (),
        }
        match stage {
            Stage::Headers => {
                state.snapshot.attempts = state.snapshot.attempts.saturating_add(1);
                state.snapshot.response_status = None;
            }
            Stage::RetryWait => {
                state.snapshot.retry_waits = state.snapshot.retry_waits.saturating_add(1)
            }
            Stage::Idle | Stage::Body => (),
        }
        state.snapshot.stage = stage;
        state.began = now;
    }
    pub(crate) fn received(&self, status: u16) {
        if let Ok(mut state) = self.0.lock() {
            state.snapshot.response_status = Some(status);
        }
    }
    pub(crate) fn note_reauthentication(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.snapshot.reauthentications = state.snapshot.reauthentications.saturating_add(1);
        }
    }
    pub(crate) fn failed(&self, timeout: bool, connect: bool) {
        let Ok(mut state) = self.0.lock() else {
            return;
        };
        let snapshot = &mut state.snapshot;
        snapshot.network_failures = snapshot.network_failures.saturating_add(1);
        snapshot.timeouts = snapshot.timeouts.saturating_add(u64::from(timeout));
        snapshot.connect_failures = snapshot.connect_failures.saturating_add(u64::from(connect));
    }
    pub fn snapshot(&self) -> Option<Snapshot> {
        self.snapshot_at(Instant::now())
    }
    fn snapshot_at(&self, now: Instant) -> Option<Snapshot> {
        self.0.lock().ok().map(|state| Snapshot {
            elapsed: if state.snapshot.stage == Stage::Idle {
                Duration::ZERO
            } else {
                now.saturating_duration_since(state.began)
            },
            ..state.snapshot
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_is_kept_for_body_or_backoff_and_cleared_before_the_next_attempt() {
        let measurements = Measurements::new();
        assert!(measurements.snapshot().unwrap().response_status.is_none());
        measurements.enter(Stage::Headers);
        measurements.received(503);
        measurements.enter(Stage::RetryWait);
        assert_eq!(measurements.snapshot().unwrap().response_status, Some(503));
        measurements.enter(Stage::Headers);
        assert!(measurements.snapshot().unwrap().response_status.is_none());
        measurements.received(200);
        measurements.enter(Stage::Body);
        measurements.failed(true, false);
        measurements.enter(Stage::Idle);
        assert_eq!(measurements.snapshot().unwrap().response_status, Some(200));
        measurements.enter(Stage::Headers);
        measurements.failed(true, false);
        measurements.enter(Stage::Idle);
        assert!(measurements.snapshot().unwrap().response_status.is_none());
    }
    #[test]
    fn failure_counters_keep_overlapping_categories_and_saturate_without_history() {
        let measurements = Measurements::new();
        measurements.failed(false, false);
        measurements.failed(true, false);
        measurements.failed(true, true);
        let snapshot = measurements.snapshot().unwrap();
        assert_eq!(
            (
                snapshot.network_failures,
                snapshot.timeouts,
                snapshot.connect_failures
            ),
            (3, 2, 1)
        );
        assert_eq!((snapshot.attempts, snapshot.retry_waits), (0, 0));
        {
            let mut state = measurements.0.lock().unwrap();
            state.snapshot.network_failures = u64::MAX;
            state.snapshot.timeouts = u64::MAX;
            state.snapshot.connect_failures = u64::MAX;
        }
        measurements.failed(true, true);
        let snapshot = measurements.snapshot().unwrap();
        assert_eq!(
            (
                snapshot.network_failures,
                snapshot.timeouts,
                snapshot.connect_failures
            ),
            (u64::MAX, u64::MAX, u64::MAX)
        );
        let fresh = Measurements::new().snapshot().unwrap();
        assert_eq!(
            (
                fresh.network_failures,
                fresh.timeouts,
                fresh.connect_failures
            ),
            (0, 0, 0)
        );
    }
    #[test]
    fn idle_decode_and_retry_wait_never_inflate_request_timings() {
        let measurements = Measurements::new();
        let now = Instant::now();
        measurements.enter_at(Stage::Headers, now);
        measurements.enter_at(Stage::Body, now + Duration::from_millis(50));
        measurements.enter_at(Stage::RetryWait, now + Duration::from_millis(70));
        measurements.enter_at(Stage::Headers, now + Duration::from_secs(10));
        let active = measurements
            .snapshot_at(now + Duration::from_secs(12))
            .unwrap();
        assert_eq!(active.elapsed, Duration::from_secs(2));
        assert_eq!(active.headers_max, Duration::from_millis(50));
        assert_eq!(active.body_max, Duration::from_millis(20));
        assert_eq!((active.attempts, active.retry_waits), (2, 1));
        measurements.enter_at(Stage::Idle, now + Duration::from_secs(13));
        let idle = measurements
            .snapshot_at(now + Duration::from_secs(100))
            .unwrap();
        assert_eq!(idle.elapsed, Duration::ZERO);
        assert_eq!(idle.headers_max, Duration::from_secs(3));
        assert_eq!(idle.body_max, Duration::from_millis(20));
        assert_eq!(Measurements::new().snapshot().unwrap().attempts, 0);
    }
    #[test]
    fn reauthentication_count_saturates_without_recording_tokens() {
        let measurements = Measurements::new();
        assert_eq!(measurements.snapshot().unwrap().reauthentications, 0);
        measurements.note_reauthentication();
        measurements.note_reauthentication();
        assert_eq!(measurements.snapshot().unwrap().reauthentications, 2);
        {
            let mut state = measurements.0.lock().unwrap();
            state.snapshot.reauthentications = u64::MAX;
        }
        measurements.note_reauthentication();
        assert_eq!(measurements.snapshot().unwrap().reauthentications, u64::MAX);
        assert_eq!(Measurements::new().snapshot().unwrap().reauthentications, 0);
    }
}
