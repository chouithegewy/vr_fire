//! Relay limits (F01 backpressure) and the per-connection ingress token bucket.

use std::time::{Duration, Instant};

/// Every relay bound in one place; tests override fields, production uses `default()`.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Admitted sockets, including handshakes and closing workers.
    pub max_sockets: usize,
    /// Pending entries per recipient.
    pub outbox_capacity: usize,
    /// Largest text message in or out, bytes (inclusive).
    pub max_message: usize,
    /// Largest WebSocket frame payload, bytes.
    pub max_frame: usize,
    pub read_buffer: usize,
    pub max_write_buffer: usize,
    /// Total time allowed for one send/flush operation.
    pub write_deadline: Duration,
    pub read_poll: Duration,
    /// Absolute handshake limit from admission.
    pub handshake_deadline: Duration,
    /// Ingress application messages per second, and burst capacity.
    pub msg_rate: f64,
    pub msg_burst: f64,
    /// Outbound drain slice: at most this many messages or this long, per worker iteration.
    pub drain_max_msgs: usize,
    pub drain_max_time: Duration,
    pub idle_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_sockets: 32,
            outbox_capacity: 64,
            max_message: 4096,
            max_frame: 4096,
            read_buffer: 4096,
            max_write_buffer: 16 * 1024,
            write_deadline: Duration::from_millis(500),
            read_poll: Duration::from_millis(20),
            handshake_deadline: Duration::from_secs(5),
            msg_rate: 40.0,
            msg_burst: 40.0,
            drain_max_msgs: 32,
            drain_max_time: Duration::from_millis(2),
            idle_timeout: Duration::from_secs(60),
        }
    }
}

/// Token bucket over a monotonic clock supplied by the caller (so tests can fake time).
pub struct TokenBucket {
    rate: f64,
    burst: f64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(rate: f64, burst: f64, now: Instant) -> Self {
        TokenBucket { rate, burst, tokens: burst, last: now }
    }

    /// Spend one token; `false` means the message exceeds the rate limit.
    pub fn take(&mut self, now: Instant) -> bool {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = self.last.max(now);
        self.tokens = (self.tokens + dt * self.rate).min(self.burst);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_then_refill_at_the_configured_rate() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(40.0, 40.0, t0);
        for _ in 0..40 {
            assert!(b.take(t0));
        }
        assert!(!b.take(t0), "burst exhausted");
        // 100 ms refills 4 tokens.
        let t1 = t0 + Duration::from_millis(100);
        for _ in 0..4 {
            assert!(b.take(t1));
        }
        assert!(!b.take(t1));
        // A long pause refills only up to the burst.
        let t2 = t1 + Duration::from_secs(60);
        for _ in 0..40 {
            assert!(b.take(t2));
        }
        assert!(!b.take(t2));
    }

    #[test]
    fn normal_viewer_traffic_is_admitted() {
        // 20 Hz poses plus a 1 Hz ping for a minute, evenly spaced.
        let t0 = Instant::now();
        let mut b = TokenBucket::new(40.0, 40.0, t0);
        for ms in (0..60_000u64).step_by(50) {
            let now = t0 + Duration::from_millis(ms);
            assert!(b.take(now), "pose at {ms} ms");
            if ms % 1000 == 0 {
                assert!(b.take(now), "ping at {ms} ms");
            }
        }
    }

    #[test]
    fn a_sustained_flood_is_rejected() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(40.0, 40.0, t0);
        let admitted = (0..1000u64).filter(|i| b.take(t0 + Duration::from_millis(i * 5))).count();
        // 5 s at 200 msg/s: 40 burst + ~200 refilled.
        assert!((230..=245).contains(&admitted), "admitted {admitted}");
    }
}
