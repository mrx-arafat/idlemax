//! The outbox journal: one JSON object per line, appended before an update is sent.
//! Domain layer, pure: this module encodes and replays lines, the agent adapter owns the file.
//!
//! `{"update":{...}}` records a queued update, `{"ack":N}` forgets every update up to `N`.
//! A crash can tear the last line; replay skips any line it cannot read.

use serde::{Deserialize, Serialize};

use crate::protocol::JobUpdate;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Record {
    Update(JobUpdate),
    Ack(u64),
}

/// One journal line for a queued update, newline included.
pub fn encode_update(update: &JobUpdate) -> Result<String, serde_json::Error> {
    line(&Record::Update(update.clone()))
}

/// One journal line for a cumulative ack, newline included.
pub fn encode_ack(seq: u64) -> Result<String, serde_json::Error> {
    line(&Record::Ack(seq))
}

fn line(record: &Record) -> Result<String, serde_json::Error> {
    let mut text = serde_json::to_string(record)?;
    text.push('\n');
    Ok(text)
}

/// What a journal file held when the agent started.
#[derive(Debug, Default, PartialEq)]
pub struct Replayed {
    /// Updates no ack covers, in the order they were queued.
    pub pending: Vec<JobUpdate>,
    /// Highest `seq` in any record, so new sequence numbers never collide with old ones.
    pub max_seq: u64,
    /// Lines that could not be read, typically a last line torn by a crash.
    pub skipped: usize,
}

pub fn replay(bytes: &[u8]) -> Replayed {
    let mut updates = Vec::new();
    let (mut max_seq, mut acked, mut skipped) = (0, 0, 0);
    for raw in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        match serde_json::from_slice::<Record>(raw) {
            Ok(Record::Update(u)) => {
                max_seq = max_seq.max(u.seq);
                updates.push(u);
            }
            Ok(Record::Ack(seq)) => {
                max_seq = max_seq.max(seq);
                acked = acked.max(seq);
            }
            Err(_) => skipped += 1,
        }
    }
    updates.retain(|u| u.seq > acked);
    Replayed {
        pending: updates,
        max_seq,
        skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::JobStatus;

    fn update(seq: u64, id: i64, status: JobStatus) -> JobUpdate {
        JobUpdate {
            seq,
            id,
            status,
            exit_code: Some(0),
            output_tail: Some("tail \u{e9}".into()),
            ..JobUpdate::default()
        }
    }

    #[test]
    fn replay_keeps_unacked_updates_and_skips_a_torn_last_line() {
        let mut file = String::new();
        for (seq, id) in [(10, 1), (11, 2), (12, 3)] {
            file += &encode_update(&update(seq, id, JobStatus::Succeeded)).unwrap();
        }
        file += &encode_ack(11).unwrap();
        file += &encode_update(&update(13, 4, JobStatus::Failed)).unwrap();
        let mut bytes = file.into_bytes();
        // Crash mid-write of the next record: cut inside a multi-byte character.
        let torn = encode_update(&update(14, 5, JobStatus::Succeeded)).unwrap();
        bytes.extend_from_slice(&torn.as_bytes()[..torn.find('\u{e9}').unwrap() + 1]);

        let replayed = replay(&bytes);

        let pending: Vec<(u64, i64)> = replayed.pending.iter().map(|u| (u.seq, u.id)).collect();
        assert_eq!(pending, vec![(12, 3), (13, 4)]);
        assert_eq!(
            replayed.pending[0].output_tail.as_deref(),
            Some("tail \u{e9}")
        );
        assert_eq!((replayed.max_seq, replayed.skipped), (13, 1));
    }
}
