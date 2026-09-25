//! Bounded per-recipient outbound queue (F01 backpressure).
//!
//! Poses (`t: "s"`) from the same sender replace each other in place: a slow reader gets the
//! current state instead of a backlog. Control events (`over`, `leave`) stay FIFO and are never
//! dropped to make room; a queue that would exceed capacity closes its recipient instead.
//! Every operation is a bounded in-memory step under one short mutex; nothing here does I/O.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Why a connection is being closed (diagnostics and `/stats.json` counters).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CloseReason {
    OutboxFull,
    RateLimit,
    WriteTimeout,
    WriteError,
    ReadError,
    Idle,
    ClientClose,
    TooLarge,
}

impl CloseReason {
    pub const ALL: [CloseReason; 8] = [
        CloseReason::OutboxFull,
        CloseReason::RateLimit,
        CloseReason::WriteTimeout,
        CloseReason::WriteError,
        CloseReason::ReadError,
        CloseReason::Idle,
        CloseReason::ClientClose,
        CloseReason::TooLarge,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            CloseReason::OutboxFull => "outbox_full",
            CloseReason::RateLimit => "rate_limit",
            CloseReason::WriteTimeout => "write_timeout",
            CloseReason::WriteError => "write_error",
            CloseReason::ReadError => "read_error",
            CloseReason::Idle => "idle",
            CloseReason::ClientClose => "client_close",
            CloseReason::TooLarge => "too_large",
        }
    }
}

/// One pending message, serialized once and shared by every recipient.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outbound {
    Pose { sender: u64, text: Arc<str> },
    Event { text: Arc<str> },
}

impl Outbound {
    pub fn text(&self) -> &Arc<str> {
        match self {
            Outbound::Pose { text, .. } | Outbound::Event { text } => text,
        }
    }
}

/// Result of an enqueue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Push {
    Queued,
    /// Replaced this sender's pending pose; the queue did not grow.
    Replaced,
    /// This enqueue overflowed: the recipient is now closing and its queue was cleared.
    /// The caller must request the recipient's socket shutdown (outside any lock).
    Overflow,
    /// The recipient was already closing; nothing was queued.
    Closed,
}

struct Inner {
    queue: VecDeque<Outbound>,
    closing: Option<CloseReason>,
    high_water: usize,
    coalesced: u64,
}

pub struct Outbox {
    capacity: usize,
    inner: Mutex<Inner>,
}

impl Outbox {
    pub fn new(capacity: usize) -> Self {
        let inner = Inner { queue: VecDeque::with_capacity(capacity), closing: None, high_water: 0, coalesced: 0 };
        Outbox { capacity, inner: Mutex::new(inner) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A panic while holding this lock can't leave the queue inconsistent (single-step ops).
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queue a pose, replacing a pending pose from the same sender.
    pub fn push_pose(&self, sender: u64, text: Arc<str>) -> Push {
        let mut g = self.lock();
        if g.closing.is_some() {
            return Push::Closed;
        }
        let pending = g.queue.iter_mut().find(|o| matches!(o, Outbound::Pose { sender: s, .. } if *s == sender));
        if let Some(slot) = pending {
            *slot = Outbound::Pose { sender, text };
            g.coalesced += 1;
            return Push::Replaced;
        }
        self.append(&mut g, Outbound::Pose { sender, text })
    }

    /// Queue a control event (FIFO, never replaced).
    pub fn push_event(&self, text: Arc<str>) -> Push {
        let mut g = self.lock();
        if g.closing.is_some() {
            return Push::Closed;
        }
        self.append(&mut g, Outbound::Event { text })
    }

    /// Queue `sender`'s leave: drop its pending pose first so it can't follow the leave.
    pub fn push_leave(&self, sender: u64, text: Arc<str>) -> Push {
        let mut g = self.lock();
        if g.closing.is_some() {
            return Push::Closed;
        }
        g.queue.retain(|o| !matches!(o, Outbound::Pose { sender: s, .. } if *s == sender));
        self.append(&mut g, Outbound::Event { text })
    }

    fn append(&self, g: &mut Inner, o: Outbound) -> Push {
        if g.queue.len() >= self.capacity {
            g.closing = Some(CloseReason::OutboxFull);
            g.queue.clear();
            return Push::Overflow;
        }
        g.queue.push_back(o);
        g.high_water = g.high_water.max(g.queue.len());
        Push::Queued
    }

    /// Next message to write, or `None` when empty or closing.
    pub fn pop(&self) -> Option<Outbound> {
        let mut g = self.lock();
        if g.closing.is_some() {
            return None;
        }
        g.queue.pop_front()
    }

    /// Mark closing and clear pending output. Returns `true` only for the first close.
    pub fn close(&self, reason: CloseReason) -> bool {
        let mut g = self.lock();
        g.queue.clear();
        if g.closing.is_some() {
            return false;
        }
        g.closing = Some(reason);
        true
    }

    pub fn closing(&self) -> Option<CloseReason> {
        self.lock().closing
    }

    pub fn len(&self) -> usize {
        self.lock().queue.len()
    }

    /// Largest entry count this outbox has held.
    pub fn high_water(&self) -> usize {
        self.lock().high_water
    }

    /// Poses that replaced an older pending pose (the relay counts room-wide in `Counters`).
    #[cfg(test)]
    pub fn coalesced(&self) -> u64 {
        self.lock().coalesced
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Arc<str> {
        Arc::from(s)
    }

    #[test]
    fn the_first_nonreplaceable_entry_past_capacity_closes_only_that_outbox() {
        let a = Outbox::new(64);
        let b = Outbox::new(64);
        for i in 0..64 {
            assert_eq!(a.push_event(t(&format!("e{i}"))), Push::Queued);
        }
        assert_eq!(a.len(), 64);
        assert_eq!(a.push_event(t("e64")), Push::Overflow);
        assert_eq!(a.closing(), Some(CloseReason::OutboxFull));
        assert_eq!(a.len(), 0, "pending output is cleared");
        assert_eq!(a.push_event(t("late")), Push::Closed);
        assert_eq!(a.push_pose(1, t("late pose")), Push::Closed);
        assert_eq!(a.pop(), None);
        assert_eq!(a.high_water(), 64);
        assert_eq!(b.push_event(t("fine")), Push::Queued);
        assert_eq!(b.closing(), None);
    }

    #[test]
    fn a_new_senders_pose_also_overflows_a_full_queue() {
        let a = Outbox::new(2);
        a.push_event(t("e0"));
        a.push_event(t("e1"));
        assert_eq!(a.push_pose(9, t("p")), Push::Overflow);
    }

    #[test]
    fn thousands_of_poses_from_one_sender_keep_one_entry_with_the_latest() {
        let a = Outbox::new(64);
        assert_eq!(a.push_pose(1, t("p0")), Push::Queued);
        for i in 1..5000 {
            assert_eq!(a.push_pose(1, t(&format!("p{i}"))), Push::Replaced);
        }
        assert_eq!(a.len(), 1);
        assert_eq!(a.coalesced(), 4999);
        assert_eq!(a.pop(), Some(Outbound::Pose { sender: 1, text: t("p4999") }));
    }

    #[test]
    fn replacement_works_at_full_capacity_and_keeps_position() {
        let a = Outbox::new(4);
        a.push_event(t("e0"));
        a.push_pose(2, t("old"));
        a.push_event(t("e1"));
        a.push_pose(3, t("x"));
        assert_eq!(a.len(), 4);
        assert_eq!(a.push_pose(2, t("new")), Push::Replaced);
        assert_eq!(a.len(), 4);
        let order: Vec<_> = std::iter::from_fn(|| a.pop()).map(|o| o.text().to_string()).collect();
        assert_eq!(order, ["e0", "new", "e1", "x"]);
    }

    #[test]
    fn control_events_stay_fifo_and_leave_purges_the_senders_pose() {
        let a = Outbox::new(64);
        a.push_pose(1, t("pose1"));
        a.push_pose(2, t("pose2"));
        a.push_event(t("over"));
        assert_eq!(a.push_leave(1, t("leave1")), Push::Queued);
        let order: Vec<_> = std::iter::from_fn(|| a.pop()).map(|o| o.text().to_string()).collect();
        assert_eq!(order, ["pose2", "over", "leave1"]);
    }

    #[test]
    fn leave_fits_when_it_purges_a_pose_from_a_full_queue() {
        let a = Outbox::new(2);
        a.push_event(t("e0"));
        a.push_pose(1, t("p1"));
        assert_eq!(a.push_leave(1, t("leave1")), Push::Queued);
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn close_is_idempotent_and_clears() {
        let a = Outbox::new(8);
        a.push_event(t("e"));
        assert!(a.close(CloseReason::Idle));
        assert!(!a.close(CloseReason::WriteError));
        assert_eq!(a.closing(), Some(CloseReason::Idle));
        assert_eq!(a.len(), 0);
    }

    #[test]
    fn concurrent_producers_never_exceed_capacity() {
        let a = Arc::new(Outbox::new(64));
        let threads: Vec<_> = (0..8u64)
            .map(|s| {
                let a = a.clone();
                std::thread::spawn(move || {
                    for i in 0..2000 {
                        if i % 50 == 0 {
                            a.push_event(t("ev"));
                        } else {
                            a.push_pose(s, t("p"));
                        }
                        assert!(a.len() <= 64);
                    }
                })
            })
            .collect();
        for th in threads {
            th.join().unwrap();
        }
        assert!(a.high_water() <= 64);
        assert_eq!(a.closing(), Some(CloseReason::OutboxFull), "8 × 40 events overflow a 64-entry queue");
        assert_eq!(a.len(), 0);
    }
}
