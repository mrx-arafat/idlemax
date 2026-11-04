//! Job updates waiting for the coordinator, acked by sequence number. Domain layer, pure.

use std::collections::VecDeque;

use crate::protocol::JobUpdate;

/// Updates in send order. When full it drops the oldest non-terminal update, so exit
/// codes survive a long outage.
#[derive(Debug, Clone)]
pub struct Outbox {
    queue: VecDeque<(u64, JobUpdate)>,
    next_seq: u64,
    cap: usize,
    dropped: u64,
}

impl Outbox {
    pub fn new(cap: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            next_seq: 0,
            cap: cap.max(1),
            dropped: 0,
        }
    }

    pub fn push(&mut self, update: JobUpdate) {
        if self.queue.len() >= self.cap {
            let victim = self
                .queue
                .iter()
                .position(|(_, u)| !u.status.is_terminal())
                .unwrap_or(0);
            self.queue.remove(victim);
            self.dropped += 1;
        }
        self.queue.push_back((self.next_seq, update));
        self.next_seq += 1;
    }

    /// Up to `max` oldest updates with their sequence numbers, for one send attempt.
    pub fn pending(&self, max: usize) -> Vec<(u64, JobUpdate)> {
        self.queue.iter().take(max).cloned().collect()
    }

    /// Forgets every update up to and including `seq` (delivered or rejected).
    pub fn ack(&mut self, seq: u64) {
        while self.queue.front().is_some_and(|(s, _)| *s <= seq) {
            self.queue.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Updates lost to the cap since start.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}
