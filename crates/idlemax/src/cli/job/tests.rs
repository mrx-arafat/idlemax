//! Tests: the `job add` command line and the text that confirms a queued job.

use clap::Parser;
use idlemax_core::types::JobStatus;

use super::super::commands::JobCmd;
use super::super::{Cli, Cmd};
use super::*;

fn parse(args: &[&str]) -> Result<AddArgs, clap::Error> {
    let argv = ["idlemax", "job", "add"]
        .into_iter()
        .chain(args.iter().copied());
    match Cli::try_parse_from(argv)?.cmd {
        Some(Cmd::Job(JobCmd::Add(add))) => Ok(*add),
        other => panic!("parsed as {other:?}"),
    }
}

#[test]
fn file_specs_reject_misspelled_resource_limits() {
    for key in ["mem", "max_runtime"] {
        let text = format!("name = 'test'\ncommand = ['/bin/echo']\n{key} = 2048\n");
        let err = parse_spec(&text, "stdin", "<file>").unwrap_err();
        let error = err.downcast_ref::<CliError>().expect("usage error");
        assert_eq!(error.code, 2);
        assert!(error.what.contains(key), "{error}");
    }
    assert!(parse_spec(
        "name = 'test'\ncommand = ['/bin/echo']\nevery = '1h'\nmem_mb = 64\n",
        "stdin",
        "<file>"
    )
    .is_ok());
}

#[test]
fn flags_a_spec_file_and_stdin_are_three_ways_to_say_the_same_thing() {
    let flags = parse(&[
        "--name",
        "t",
        "--priority",
        "-5",
        "--wait",
        "--timeout",
        "9",
        "--",
        "cargo",
        "test",
        "--lib",
    ])
    .unwrap();
    assert_eq!(flags.argv, ["cargo", "test", "--lib"]);
    assert_eq!(
        (flags.priority, flags.timeout, flags.wait),
        (Some(-5), Some(9), true)
    );

    assert_eq!(
        parse(&["spec.toml", "--key", "k"]).unwrap().spec,
        Some("spec.toml".into())
    );
    assert_eq!(parse(&["-"]).unwrap().spec, Some("-".into()));
}

#[test]
fn mixing_a_spec_with_flags_or_a_command_is_a_usage_error() {
    for bad in [
        &["spec.toml", "--name", "x"][..],
        &["spec.toml", "--", "true"],
        &["--timeout", "5", "--", "true"],
        &["--mem", "0", "--", "true"],
    ] {
        let e = parse(bad).unwrap_err();
        assert_eq!(e.exit_code(), 2, "{bad:?}");
    }
}

#[test]
fn queueing_says_what_to_run_next_and_flags_a_no_op() {
    let job = Job {
        id: 7,
        status: JobStatus::Running,
        spec: JobSpec {
            name: "tests".into(),
            ..JobSpec::default()
        },
        ..Job::default()
    };
    let new = queued_text(&job, true);
    assert_eq!(
        new,
        "job 7 running: tests\nnext: `idlemax why 7` (why it waits), `idlemax logs 7` \
         (its output), `idlemax job cancel 7` (stop it)"
    );
    assert!(queued_text(&job, false).starts_with("job 7 running: tests - already queued (no-op)\n"));
}
