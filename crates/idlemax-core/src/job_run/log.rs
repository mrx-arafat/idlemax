//! Domain: recognize the latest attempt's completion in a byte stream, across read boundaries.

const HEADER: &[u8] = b"=== idlemax job ";
const FOOTER: &[u8] = b"=== idlemax: ";

/// Tracks reserved log markers using only a line prefix, regardless of output size.
#[derive(Debug, Default)]
pub struct LogProgress {
    prefix: Vec<u8>,
    finished: bool,
}

impl LogProgress {
    /// Consumes bytes and reports whether the latest complete attempt has ended.
    pub fn push(&mut self, bytes: &[u8]) -> bool {
        for byte in bytes {
            if *byte == b'\n' {
                if self.prefix.starts_with(HEADER) {
                    self.finished = false;
                } else if self.prefix.starts_with(FOOTER) {
                    self.finished = true;
                }
                self.prefix.clear();
            } else if self.prefix.len() < HEADER.len() {
                self.prefix.push(*byte);
            }
        }
        // A partial line may be a new attempt header or a footer split across reads.
        self.finished && self.prefix.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_latest_attempt_owns_completion_regardless_of_read_boundaries() {
        let header = b"=== idlemax job 10 attempt 2 started fixture ===\n\xffready\n";
        let footer = b"=== idlemax: succeeded (exit 0) ===\n";
        for split in 0..footer.len() {
            let mut progress = LogProgress::default();
            assert!(progress.push(b"=== idlemax: killed (exit none) ===\n"));
            assert!(!progress.push(&header[..5]));
            assert!(!progress.push(&header[5..]));
            assert!(!progress.push(&footer[..split]));
            assert!(progress.push(&footer[split..]));
        }
    }
}
