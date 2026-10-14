//! Circuit breakers with explicit time.
//!
//! Mirrors the JS breaker registry semantics (`CLOSED` serving, `OPEN`
//! shedding after `threshold` consecutive failures, half-open trial after
//! `cooldown`). Time is injected (`std::time::Instant` parameters) so tests
//! never sleep.
//!
//! The half-open trial is **single-flight**: when the cooldown elapses exactly
//! one request is admitted as the probe, every other in-flight request keeps
//! falling back to the next candidate, and only the probe's outcome may close
//! the breaker. Admission doubles as the authority token that `record_success`
//! checks: a trip (`record_failure`) or a successful probe revokes it, so a
//! late reply from a request that started *before* the trip cannot resurrect a
//! backend that is still failing (the flapping/lost-trip bug).

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    /// Serving normally.
    Closed,
    /// Shedding load until the cooldown elapses.
    Open,
    /// The cooldown elapsed and the single trial request is in flight.
    HalfOpen,
}

/// Consecutive-failure breaker for one backend.
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    threshold: u32,
    cooldown: Duration,
    failures: u32,
    opened_at: Option<Instant>,
    /// When the half-open probe was admitted, if one is in flight. Doubles as
    /// the single-flight gate (one probe at a time) and as the authority token
    /// `record_success` requires before it may close the breaker.
    probe: Option<Instant>,
}

impl CircuitBreaker {
    /// Build a breaker that opens after `threshold` consecutive failures and
    /// allows a probe after `cooldown_secs`.
    pub fn new(threshold: u32, cooldown_secs: u64) -> Self {
        Self {
            threshold: threshold.max(1),
            cooldown: Duration::from_secs(cooldown_secs),
            failures: 0,
            opened_at: None,
            probe: None,
        }
    }

    /// Raw state.
    pub fn state(&self) -> BreakerState {
        match (self.opened_at, self.probe) {
            (Some(_), Some(_)) => BreakerState::HalfOpen,
            (Some(_), None) => BreakerState::Open,
            (None, _) => BreakerState::Closed,
        }
    }

    /// Whether a request may proceed now.
    ///
    /// A closed breaker admits everything. An open breaker admits nothing
    /// until the cooldown elapses, and then hands the *single* trial request to
    /// the first caller that asks: while that probe is in flight every other
    /// caller is refused and falls back to the next candidate. The probe is
    /// forgotten if it reports nothing for a whole cooldown, so a lost reply
    /// can never pin the backend out of rotation permanently.
    pub fn allow(&mut self, now: Instant) -> bool {
        let Some(opened) = self.opened_at else {
            return true;
        };
        if let Some(admitted) = self.probe
            && now.duration_since(admitted) < self.cooldown
        {
            return false;
        }
        if now.duration_since(opened) < self.cooldown {
            return false;
        }
        self.probe = Some(now);
        true
    }

    /// Record a success.
    ///
    /// Only a success carrying an outstanding probe's authority — i.e. a
    /// half-open breaker — closes the breaker. A success for an open breaker
    /// came from a request admitted before the trip, and revoking nothing
    /// means it is dropped: the cooldown keeps running instead of being reset
    /// by luck. A success for an already closed breaker cannot change state; it
    /// only clears the consecutive-failure counter, which is what makes the
    /// threshold count *consecutive* failures.
    pub fn record_success(&mut self) {
        if self.probe.is_some() {
            self.probe = None;
            self.opened_at = None;
        }
        if self.opened_at.is_none() {
            self.failures = 0;
        }
    }

    /// Record a failure: open once the threshold is reached (restarting the
    /// cooldown on every failure while open). Any failure ends the probe
    /// whatever its result, so the cooldown restarts from now and the outcome
    /// of a probe the failure raced can no longer close the breaker.
    pub fn record_failure(&mut self, now: Instant) {
        self.failures += 1;
        self.probe = None;
        if self.failures >= self.threshold {
            self.opened_at = Some(now);
        }
    }
}

/// Named breakers, one per backend.
#[derive(Debug, Default)]
pub struct BreakerSet {
    breakers: HashMap<String, CircuitBreaker>,
    threshold: u32,
    cooldown_secs: u64,
}

impl BreakerSet {
    /// Empty set; breakers are created lazily with these settings.
    pub fn new(threshold: u32, cooldown_secs: u64) -> Self {
        Self {
            breakers: HashMap::new(),
            threshold,
            cooldown_secs,
        }
    }

    fn breaker(&mut self, backend: &str) -> &mut CircuitBreaker {
        self.breakers
            .entry(backend.to_string())
            .or_insert_with(|| CircuitBreaker::new(self.threshold, self.cooldown_secs))
    }

    /// Whether `backend` may serve now.
    pub fn allow(&mut self, backend: &str, now: Instant) -> bool {
        self.breaker(backend).allow(now)
    }

    /// Record a success for `backend`.
    pub fn record_success(&mut self, backend: &str) {
        self.breaker(backend).record_success();
    }

    /// Record a failure for `backend`.
    pub fn record_failure(&mut self, backend: &str, now: Instant) {
        self.breaker(backend).record_failure(now);
    }

    /// Raw state for inspection.
    pub fn state(&mut self, backend: &str) -> BreakerState {
        self.breaker(backend).state()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_after_threshold_and_recovers() {
        let start = Instant::now();
        let mut breaker = CircuitBreaker::new(3, 60);

        assert!(breaker.allow(start));
        breaker.record_failure(start);
        breaker.record_failure(start);
        assert_eq!(breaker.state(), BreakerState::Closed);
        assert!(breaker.allow(start));

        breaker.record_failure(start);
        assert_eq!(breaker.state(), BreakerState::Open);
        assert!(!breaker.allow(start));

        // Cooldown elapsed: the first caller takes the single trial probe, the
        // next one is refused and falls back to the next candidate.
        let later = start + Duration::from_secs(61);
        assert!(breaker.allow(later));
        assert_eq!(breaker.state(), BreakerState::HalfOpen);
        assert!(!breaker.allow(later));

        breaker.record_success();
        assert_eq!(breaker.state(), BreakerState::Closed);
        assert!(breaker.allow(later));
    }

    /// The probe's outcome decides: a failure re-opens the breaker and restarts
    /// the cooldown from now, so the herd stays out for another full window.
    #[test]
    fn a_failing_probe_reopens_the_breaker() {
        let start = Instant::now();
        let mut breaker = CircuitBreaker::new(1, 60);
        breaker.record_failure(start);

        let probe_at = start + Duration::from_secs(61);
        assert!(breaker.allow(probe_at));
        assert_eq!(breaker.state(), BreakerState::HalfOpen);

        breaker.record_failure(probe_at + Duration::from_secs(1));
        assert_eq!(breaker.state(), BreakerState::Open);
        assert!(!breaker.allow(probe_at + Duration::from_secs(2)));
        assert!(
            breaker.allow(probe_at + Duration::from_secs(61)),
            "the cooldown must restart at the probe's failure"
        );
    }

    /// The other half of the probe outcome: a success from the admitted trial
    /// closes the breaker and clears the consecutive-failure counter.
    #[test]
    fn a_successful_probe_closes_the_breaker() {
        let start = Instant::now();
        let mut breaker = CircuitBreaker::new(3, 60);
        breaker.record_failure(start);
        breaker.record_failure(start);
        breaker.record_failure(start);
        assert_eq!(breaker.state(), BreakerState::Open);

        let probe_at = start + Duration::from_secs(61);
        assert!(breaker.allow(probe_at));
        breaker.record_success();

        assert_eq!(breaker.state(), BreakerState::Closed);
        // Fully recovered: one failure short of the threshold again.
        breaker.record_failure(start);
        breaker.record_failure(start);
        assert_eq!(breaker.state(), BreakerState::Closed);
        breaker.record_failure(start);
        assert_eq!(breaker.state(), BreakerState::Open);
    }

    /// A success for a closed breaker cannot change state, but it does clear
    /// the consecutive-failure counter — that is what makes the threshold
    /// count consecutive failures.
    #[test]
    fn a_success_while_closed_resets_the_failure_streak() {
        let start = Instant::now();
        let mut breaker = CircuitBreaker::new(3, 60);
        for _ in 0..2 {
            breaker.record_failure(start);
            breaker.record_success();
        }
        assert_eq!(breaker.state(), BreakerState::Closed);
        breaker.record_failure(start);
        assert_eq!(breaker.state(), BreakerState::Closed);
    }

    /// A probe that reports nothing must not pin the backend out of rotation.
    #[test]
    fn an_abandoned_probe_expires_with_the_cooldown() {
        let start = Instant::now();
        let mut breaker = CircuitBreaker::new(1, 60);
        breaker.record_failure(start);

        let probe_at = start + Duration::from_secs(61);
        assert!(breaker.allow(probe_at));
        assert!(!breaker.allow(start + Duration::from_secs(100)));
        // Same probe, a whole cooldown later: it is considered lost.
        assert!(breaker.allow(start + Duration::from_secs(121)));
    }

    #[test]
    fn failure_while_open_restarts_cooldown() {
        let start = Instant::now();
        let mut breaker = CircuitBreaker::new(1, 60);
        breaker.record_failure(start);
        assert!(!breaker.allow(start + Duration::from_secs(30)));

        breaker.record_failure(start + Duration::from_secs(30));
        // Cooldown restarted at t=30, so t=61 is still inside.
        assert!(!breaker.allow(start + Duration::from_secs(61)));
        assert!(breaker.allow(start + Duration::from_secs(91)));
    }

    /// RED (defect 1a): once the cooldown elapses the breaker must admit ONE
    /// trial request, not every request that happens to be in flight at that
    /// moment. Eight callers racing on one tripped backend must produce one
    /// probe, seven rejections (which fall back to the next candidate).
    #[test]
    fn cooldown_admits_one_probe_not_the_whole_herd() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier, Mutex};

        let start = Instant::now();
        let set = Arc::new(Mutex::new(BreakerSet::new(1, 60)));
        set.lock().unwrap().record_failure("a", start);

        const CALLERS: usize = 8;
        let after = start + Duration::from_secs(61);
        let barrier = Arc::new(Barrier::new(CALLERS));
        let admitted = Arc::new(AtomicUsize::new(0));
        let mut callers = Vec::new();
        for _ in 0..CALLERS {
            let set = Arc::clone(&set);
            let barrier = Arc::clone(&barrier);
            let admitted = Arc::clone(&admitted);
            callers.push(std::thread::spawn(move || {
                barrier.wait();
                if set.lock().unwrap().allow("a", after) {
                    admitted.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }
        for caller in callers {
            caller.join().unwrap();
        }

        assert_eq!(
            admitted.load(Ordering::SeqCst),
            1,
            "a just-tripped backend must serve one probe, not the whole herd"
        );
    }

    /// RED (defect 1b): a success recorded for a request that was dispatched
    /// BEFORE another request tripped the breaker must not close it. Otherwise
    /// one lucky in-flight reply resurrects a backend that is still failing and
    /// the breaker flaps open/closed under load.
    #[test]
    fn stale_success_does_not_resurrect_a_tripped_breaker() {
        let start = Instant::now();
        let mut breaker = CircuitBreaker::new(1, 60);

        // This request was admitted while the breaker was closed.
        assert!(breaker.allow(start));
        // A concurrent request trips the breaker.
        breaker.record_failure(start);
        assert_eq!(breaker.state(), BreakerState::Open);

        // Now the pre-trip request answers 200.
        breaker.record_success();
        assert_eq!(
            breaker.state(),
            BreakerState::Open,
            "a stale success closed a breaker a later failure re-opened"
        );
        assert!(
            !breaker.allow(start + Duration::from_secs(1)),
            "the cooldown must not restart early"
        );
    }

    #[test]
    fn set_tracks_backends_independently() {
        let start = Instant::now();
        let mut set = BreakerSet::new(1, 60);
        set.record_failure("a", start);
        assert_eq!(set.state("a"), BreakerState::Open);
        assert_eq!(set.state("b"), BreakerState::Closed);
        assert!(set.allow("b", start));
        // `b` closing must not be attributed to `a`, and only a probe can close
        // `a` at all.
        set.record_success("b");
        assert_eq!(set.state("a"), BreakerState::Open);

        let after = start + Duration::from_secs(61);
        assert!(set.allow("a", after));
        set.record_success("a");
        assert_eq!(set.state("a"), BreakerState::Closed);
    }
}
