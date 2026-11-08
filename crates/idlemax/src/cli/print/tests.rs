//! Tests: the one-line cluster summary names every kind of finished-badly job.

use idlemax_core::protocol::{ClusterStatus, JobCounts};

use super::summary;

fn line(jobs: JobCounts) -> String {
    summary(&ClusterStatus {
        jobs,
        ..ClusterStatus::default()
    })
}

#[test]
fn the_summary_counts_failed_killed_and_parked_jobs_when_there_are_any() {
    let busy = line(JobCounts {
        queued: 2,
        parked: 1,
        failed: 3,
        killed: 4,
        ..JobCounts::default()
    });
    assert!(
        busy.contains("jobs: 0 running, 2 queued, 1 parked, 3 failed, 4 killed |"),
        "{busy}"
    );
    let quiet = line(JobCounts::default());
    assert!(quiet.contains("jobs: 0 running, 0 queued |"), "{quiet}");
}
