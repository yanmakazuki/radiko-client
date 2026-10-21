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
    pub elapsed: Duration,
    pub headers_max: Duration,
    pub body_max: Duration,
    pub attempts: u64,
    /// Backoffs entered, including a backoff cancelled before the next request.
    pub retry_waits: u64,
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
                elapsed: Duration::ZERO,
                headers_max: Duration::ZERO,
                body_max: Duration::ZERO,
                attempts: 0,
                retry_waits: 0,
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
            Stage::Headers => state.snapshot.attempts = state.snapshot.attempts.saturating_add(1),
            Stage::RetryWait => {
                state.snapshot.retry_waits = state.snapshot.retry_waits.saturating_add(1)
            }
            Stage::Idle | Stage::Body => (),
        }
        state.snapshot.stage = stage;
        state.began = now;
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
}
