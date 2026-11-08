//! Tests: `idlemax job add` flags become a valid JobSpec, or an error that names the fix.

use std::path::Path;

use super::*;

fn draft(argv: &[&str]) -> JobDraft {
    JobDraft {
        argv: argv.iter().map(|a| (*a).to_string()).collect(),
        ..JobDraft::default()
    }
}

const BASE: &str = "/work/repo";

fn build(d: JobDraft) -> Result<JobSpec, DraftError> {
    d.into_spec(Path::new(BASE))
}

#[test]
fn a_bare_command_gets_the_spec_defaults_and_its_file_name_as_the_name() {
    let spec = build(draft(&["/usr/bin/cargo", "test", "-q"])).unwrap();
    let expected = JobSpec {
        name: "cargo".into(),
        command: vec!["/usr/bin/cargo".into(), "test".into(), "-q".into()],
        ..JobSpec::default()
    };
    assert_eq!(spec, expected);
}

#[test]
fn every_flag_lands_in_its_spec_field() {
    let spec = build(JobDraft {
        name: Some("suite".into()),
        cost: Some(CostClass::CpuHeavy),
        devices: vec!["mac".into(), "srv".into()],
        requires: vec!["cargo".into()],
        mem_mb: Some(2048),
        max_runtime_secs: Some(600),
        priority: Some(-5),
        key: Some("pr-9".into()),
        cwd: Some("/srv/app".into()),
        ..draft(&["cargo", "test"])
    })
    .unwrap();
    assert_eq!(spec.name, "suite");
    assert_eq!(spec.cost, CostClass::CpuHeavy);
    assert_eq!(spec.devices, ["mac", "srv"]);
    assert_eq!(spec.requires, ["cargo"]);
    assert_eq!((spec.mem_mb, spec.max_runtime_secs), (2048, 600));
    assert_eq!(spec.priority, -5);
    assert_eq!(spec.key.as_deref(), Some("pr-9"));
    assert_eq!(spec.cwd.as_deref(), Some("/srv/app"));
}

#[test]
fn cwd_is_made_absolute_from_the_current_directory_without_touching_the_disk() {
    let cwd = |typed: &str| {
        build(JobDraft {
            cwd: Some(typed.into()),
            ..draft(&["true"])
        })
        .unwrap()
        .cwd
        .unwrap()
    };
    assert_eq!(cwd("."), "/work/repo");
    assert_eq!(cwd("./crates/../docs"), "/work/repo/docs");
    assert_eq!(
        cwd("/elsewhere/not/on/this/machine"),
        "/elsewhere/not/on/this/machine"
    );
    assert_eq!(cwd("../../.."), "/");
}

#[test]
fn an_ai_account_means_an_ai_job_and_mismatches_say_what_to_change() {
    let ai = |cost, account: Option<&str>| {
        build(JobDraft {
            cost,
            ai_account: account.map(Into::into),
            ..draft(&["claude"])
        })
    };
    let spec = ai(None, Some("claude-max")).unwrap();
    assert_eq!(spec.cost, CostClass::Ai);

    let e = ai(Some(CostClass::Io), Some("claude-max")).unwrap_err();
    assert_eq!(e, DraftError::AccountNeedsAiCost);
    assert!(e.to_string().contains("--cost ai"), "{e}");
    let e = ai(Some(CostClass::Ai), None).unwrap_err();
    assert_eq!(e, DraftError::AiCostNeedsAccount);
    assert!(e.to_string().contains("--ai-account"), "{e}");
}

#[test]
fn no_command_points_at_the_double_dash() {
    let e = build(draft(&[])).unwrap_err();
    assert_eq!(e, DraftError::NoCommand);
    assert!(e.to_string().contains("after `--`"), "{e}");
    assert_eq!(
        build(JobDraft {
            key: Some(" ".into()),
            ..draft(&["true"])
        }),
        Err(DraftError::Spec(SpecError::EmptyKey))
    );
}
