//! Job updates waiting for the coordinator, acked by sequence number. Domain layer, pure.
//! The agent adapter journals every push so a crash loses nothing (see `journal`).

use std::collections::VecDeque;

use crate::protocol::JobUpdate;

/// Updates in send order, each stamped with its `seq` (1 and up; 0 means unsequenced on
/// the wire). When full it drops the oldest non-terminal update, so exit codes survive a
/// long outage.
#[derive(Debug, Clone)]
pub struct Outbox {
    queue: VecDeque<JobUpdate>,
    next_seq: u64,
    cap: usize,
    dropped: u64,
}

impl Outbox {
    pub fn new(cap: usize) -> Self {
        Self::restore(cap, 1, Vec::new())
    }

    /// Rebuilds the queue from journaled updates, which keep their `seq`; new updates
    /// continue at `next_seq`, which must exceed every restored `seq`.
    pub fn restore(cap: usize, next_seq: u64, updates: Vec<JobUpdate>) -> Self {
        let mut outbox = Self {
            queue: VecDeque::new(),
            next_seq: next_seq.max(1),
            cap: cap.max(1),
            dropped: 0,
        };
        for update in updates {
            outbox.enqueue(update);
        }
        outbox
    }

    /// Stamps the update with the next `seq`, queues it and returns it as queued.
    pub fn push(&mut self, mut update: JobUpdate) -> JobUpdate {
        update.seq = self.next_seq;
        self.next_seq += 1;
        self.enqueue(update.clone());
        update
    }

    fn enqueue(&mut self, update: JobUpdate) {
        if self.queue.len() >= self.cap {
            let victim = self
                .queue
                .iter()
                .position(|u| !u.status.is_terminal())
                .unwrap_or(0);
            self.queue.remove(victim);
            self.dropped += 1;
        }
        self.queue.push_back(update);
    }

    /// Up to `max` oldest updates, for one send attempt.
    pub fn pending(&self, max: usize) -> Vec<JobUpdate> {
        self.queue.iter().take(max).cloned().collect()
    }

    /// Forgets every update up to and including `seq` (delivered or rejected).
    /// Returns how many were forgotten.
    pub fn ack(&mut self, seq: u64) -> usize {
        let before = self.queue.len();
        while self.queue.front().is_some_and(|u| u.seq <= seq) {
            self.queue.pop_front();
        }
        before - self.queue.len()
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// First sequence not yet assigned, retained even when every update is forgotten.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Updates lost to the cap since start.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}
