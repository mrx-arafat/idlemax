use super::*;
use crate::types::{CostClass, JobSpec, JobStatus};

#[test]
fn render_and_parse_roundtrip() {
    let job = Job {
        id: 42,
        spec: JobSpec {
            name: "echo test".into(),
            command: vec!["/bin/echo".into(), "hello".into()],
            cost: CostClass::CpuLight,
            ..JobSpec::default()
        },
        attempts: 1,
        ..Job::default()
    };
    let outcome = Outcome {
        status: JobStatus::Succeeded,
        exit_code: Some(0),
        error: None,
    };
    let report = RunReport {
        job: &job,
        device: "laptop",
        outcome: &outcome,
        started_ms: 1730000000000,
        finished_ms: 1730000001000,
        active_ms: 1000,
        wrapper: "taskpolicy",
        log_path: "/tmp/42.log",
        output: "hello\n",
        utc_offset_secs: 0,
    };
    let rendered = render(&report);
    let fname = file_name(job.id, &job.spec.name, report.finished_ms, 0);
    let entry = parse_note(&fname, &rendered).expect("parse note");

    assert_eq!(entry.job_id, Some(42));
    assert_eq!(entry.name, "echo test");
    assert_eq!(entry.result, "succeeded");
    assert_eq!(entry.file_name, fname);
    assert!(entry.finished_ms.is_some());
}

#[test]
fn parse_failed_with_error() {
    let note = "# compile: failed\n\n- Job: 7 (attempt 2)\n- Exit code: 1\n- Error: out of memory\n- Finished: 2026-11-08 10:00:00 +00:00\n";
    let entry = parse_note("2026-11-08-7-compile.md", note).expect("parse note");
    assert_eq!(entry.job_id, Some(7));
    assert_eq!(entry.name, "compile");
    assert_eq!(entry.result, "failed: out of memory");
    assert_eq!(entry.time, "2026-11-08 10:00:00 +00:00");
}

#[test]
fn sort_newest_first() {
    let mut entries = vec![
        InboxEntry {
            time: "2026-11-08 09:00:00 +00:00".into(),
            job_id: Some(1),
            notice: None,
            name: "first".into(),
            result: "succeeded".into(),
            file_name: "f1.md".into(),
            finished_ms: Some(100),
        },
        InboxEntry {
            time: "2026-11-08 11:00:00 +00:00".into(),
            job_id: Some(3),
            notice: None,
            name: "third".into(),
            result: "succeeded".into(),
            file_name: "f3.md".into(),
            finished_ms: Some(300),
        },
        InboxEntry {
            time: "2026-11-08 10:00:00 +00:00".into(),
            job_id: Some(2),
            notice: None,
            name: "second".into(),
            result: "succeeded".into(),
            file_name: "f2.md".into(),
            finished_ms: Some(200),
        },
    ];
    sort_entries(&mut entries);
    assert_eq!(entries[0].job_id, Some(3));
    assert_eq!(entries[1].job_id, Some(2));
    assert_eq!(entries[2].job_id, Some(1));
}

#[test]
fn a_notice_note_renders_and_parses_back_and_a_job_note_keeps_its_keys() {
    use crate::notify::notice::{Notice, NoticeKind};
    let n = Notice {
        id: 12,
        at_ms: 1_800_000_000_000,
        kind: NoticeKind::TaskFinished,
        title: "idlemax: app".into(),
        body: "Fix the login test: checks passed - ready for review".into(),
    };
    let name = notice_file_name(&n, 0);
    assert_eq!(name, "2027-01-15-n12-task_finished.md");
    let text = render_notice(&n, 0);
    assert!(text.ends_with("ready for review\n"), "{text}");
    let entry = parse_note(&name, &text).unwrap();
    assert_eq!((entry.notice, entry.job_id), (Some(12), None));
    assert_eq!(entry.name, "idlemax: app");
    assert_eq!(entry.result, "task_finished");
    assert_eq!(entry.finished_ms, Some(n.at_ms));

    let note =
        "# compile: failed\n\n- Job: 7 (attempt 2)\n- Finished: 2026-11-08 10:00:00 +00:00\n";
    let job = parse_note("2026-11-08-7-compile.md", note).unwrap();
    let keys: Vec<String> = serde_json::to_value(&job)
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        keys,
        [
            "file_name",
            "finished_ms",
            "job_id",
            "name",
            "result",
            "time"
        ]
    );
}
