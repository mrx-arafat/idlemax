use std::path::Path;

use idlemax_core::protocol::{ClusterStatus, DeviceView, JobCounts};
use idlemax_core::quota::QuotaView;
use idlemax_core::types::{DeviceInfo, GuardAction, GuardDecision};

use super::render::{collapse_home, live, render, Home, Mode, Reason, State, DESCRIPTION};

fn device(id: &str, online: bool, local_paused: bool, guard: Option<GuardAction>) -> DeviceView {
    // Only a pause carries a reason, as the guard reports them.
    let reasons = match guard {
        Some(GuardAction::Pause) => vec!["pause: disk free 4 GiB".to_string()],
        _ => Vec::new(),
    };
    DeviceView {
        info: DeviceInfo {
            id: id.into(),
            ..DeviceInfo::default()
        },
        online,
        local_paused,
        guard: guard.map(|action| GuardDecision {
            action,
            reasons,
            ..GuardDecision::default()
        }),
        ..DeviceView::default()
    }
}

fn account(name: &str, spendable: bool) -> QuotaView {
    QuotaView {
        account: name.into(),
        spendable,
        ..QuotaView::default()
    }
}

fn home(state: State) -> Home {
    Home {
        bin: "~/.cargo/bin/idlemax".into(),
        description: DESCRIPTION,
        coordinator: "http://127.0.0.1:7878".into(),
        state,
    }
}

fn busy() -> State {
    let status = ClusterStatus {
        version: "0.1.0".into(),
        devices: vec![
            device(
                "mac",
                true,
                false,
                Some(GuardAction::Run {
                    slots: 2,
                    mem_mib: 512,
                }),
            ),
            device("srv", true, false, Some(GuardAction::Pause)),
            device("old", false, false, None),
            device("pc", true, true, None),
        ],
        jobs: JobCounts {
            running: 1,
            queued: 3,
            failed: 2,
            ..JobCounts::default()
        },
        open_advice: 2,
        paused_accounts: vec!["codex".into()],
        ..ClusterStatus::default()
    };
    // Two window rows for claude must give one line; codex is paused by a control.
    let quota = [
        account("claude", true),
        account("claude", true),
        account("codex", true),
    ];
    State::Up(live(&status, &quota))
}

#[test]
fn live_state_names_each_device_and_one_line_per_account() {
    let State::Up(l) = busy() else { unreachable!() };
    let states: Vec<_> = l
        .devices
        .iter()
        .map(|d| (d.id.as_str(), d.state.as_str()))
        .collect();
    assert_eq!(
        states,
        [
            ("mac", "online"),
            ("srv", "online"),
            ("old", "offline"),
            ("pc", "local-paused")
        ]
    );
    assert_eq!(l.devices[0].guard, "run 2");
    assert_eq!(l.devices[1].guard, "pause: disk free 4 GiB");
    let quota: Vec<_> = l
        .quota
        .iter()
        .map(|q| (q.account.as_str(), q.spendable))
        .collect();
    assert_eq!(quota, [("claude", true), ("codex", false)]);
}

#[test]
fn full_and_hook_views_of_a_busy_cluster_are_pinned() {
    let h = home(busy());
    let full = "\
bin: ~/.cargo/bin/idlemax
description: Run useful jobs on spare device capacity and spend leftover AI quota before it resets
coordinator: \"http://127.0.0.1:7878\"
devices[4]{id,state,guard}:
  mac,online,run 2
  srv,online,\"pause: disk free 4 GiB\"
  old,offline,-
  pc,local-paused,-
jobs: 1 running 3 queued 0 paused 2 failed 0 parked
advice: 2 open
quota[2]{account,spendable}:
  claude,true
  codex,false
help[4]:
  Run `idlemax jobs --status failed` to see the 2 failed
  Run `idlemax advise` to review 2 open
  Run `idlemax status` to see the cluster, and per device the slots and RAM in use and why it is not using more
  Run `idlemax report` to see what idlemax did today and this week, and what held it back";
    assert_eq!(render(&h, Mode::Full), full);

    let hook = "\
devices[4]{id,state,guard}:
  mac,online,run 2
  srv,online,\"pause: disk free 4 GiB\"
  old,offline,-
  pc,local-paused,-
jobs: 1 running 3 queued 2 failed
advice: 2 open
quota[2]{account,spendable}:
  claude,true
  codex,false
help[1]: Run `idlemax` for the full view";
    assert_eq!(render(&h, Mode::Hook), hook);
}

#[test]
fn the_toon_home_view_and_hook_summary_of_a_busy_cluster_are_pinned() {
    use super::view::of;
    use crate::cli::out::toon::encode;

    let h = home(busy());
    let (full, hints) = of(&h, Mode::Full);
    assert_eq!(
        encode(&full),
        "\
bin: ~/.cargo/bin/idlemax
description: Run useful jobs on spare device capacity and spend leftover AI quota before it resets
coordinator: \"http://127.0.0.1:7878\"
devices[4]{id,state,guard}:
  mac,online,run 2
  srv,online,\"pause: disk free 4 GiB\"
  old,offline,null
  pc,local-paused,null
jobs:
  running: 1
  queued: 3
  failed: 2
advice_open: 2
quota[2]{account,spendable}:
  claude,true
  codex,false"
    );
    assert_eq!(hints.len(), 4);
    assert_eq!(
        hints[0],
        "Run `idlemax jobs --status failed` to see the 2 failed"
    );

    let (hook, hints) = of(&h, Mode::Hook);
    assert_eq!(
        encode(&hook),
        "\
devices[4]{id,state,guard}:
  mac,online,run 2
  srv,online,\"pause: disk free 4 GiB\"
  old,offline,null
  pc,local-paused,null
jobs:
  running: 1
  queued: 3
  failed: 2
advice_open: 2
quota[2]{account,spendable}:
  claude,true
  codex,false
help: Run `idlemax` for the full view"
    );
    assert!(hints.is_empty());
}

#[test]
fn the_toon_notes_for_a_missing_setup_or_coordinator_are_short() {
    use super::view::of;
    use crate::cli::out::toon::encode;

    let down = home(State::Down {
        reason: Reason::Unreachable,
        detail: String::new(),
    });
    assert_eq!(
        encode(&of(&down, Mode::Hook).0),
        "idlemax: \"coordinator http://127.0.0.1:7878 is not reachable (run `idlemax doctor`)\""
    );
    let (full, hints) = of(&down, Mode::Full);
    assert!(
        encode(&full).ends_with("status: unreachable"),
        "{}",
        encode(&full)
    );
    assert!(hints[0].contains("`idlemax coord`"));

    let fresh = home(State::Down {
        reason: Reason::NotSetUp,
        detail: String::new(),
    });
    assert_eq!(
        encode(&of(&fresh, Mode::Hook).0),
        "idlemax: not set up on this device (run `idlemax setup`)"
    );

    let empty = home(State::Up(live(&ClusterStatus::default(), &[])));
    let text = encode(&of(&empty, Mode::Full).0);
    assert!(
        text.contains("devices: none have sent a heartbeat yet"),
        "{text}"
    );
    assert!(text.contains("jobs:\n  running: 0\n  queued: 0"), "{text}");
    assert!(!text.contains("quota["), "{text}");
}

#[test]
fn a_down_coordinator_is_one_short_note_with_the_next_step() {
    let unreachable = home(State::Down {
        reason: Reason::Unreachable,
        detail: String::new(),
    });
    assert_eq!(
        render(&unreachable, Mode::Hook),
        "idlemax: coordinator http://127.0.0.1:7878 is not reachable (run `idlemax doctor`)"
    );
    let full = render(&unreachable, Mode::Full);
    assert!(full.contains("idlemax: coordinator http://127.0.0.1:7878 is not reachable"));
    assert!(full.contains("help[3]:") && full.contains("`idlemax coord`"));

    let fresh = home(State::Down {
        reason: Reason::NotSetUp,
        detail: String::new(),
    });
    assert_eq!(
        render(&fresh, Mode::Hook),
        "idlemax: not set up on this device (run `idlemax setup`)"
    );
}

#[test]
fn an_empty_cluster_says_so_and_points_at_the_agent() {
    let empty = home(State::Up(live(&ClusterStatus::default(), &[])));
    let full = render(&empty, Mode::Full);
    assert!(
        full.contains("devices: none have sent a heartbeat yet"),
        "{full}"
    );
    assert!(
        full.contains("jobs: 0 running 0 queued 0 paused 0 failed 0 parked"),
        "{full}"
    );
    assert!(full.contains("Run `idlemax agent` on a device"), "{full}");
    assert!(!full.contains("quota["), "{full}");
}

#[test]
fn collapse_home_only_touches_paths_under_home() {
    let home = Path::new("/Users/me");
    assert_eq!(
        collapse_home(Path::new("/Users/me/.cargo/bin/idlemax"), Some(home)),
        "~/.cargo/bin/idlemax"
    );
    assert_eq!(
        collapse_home(Path::new("/Users/mex/bin/idlemax"), Some(home)),
        "/Users/mex/bin/idlemax"
    );
    assert_eq!(
        collapse_home(Path::new("/usr/local/bin/idlemax"), None),
        "/usr/local/bin/idlemax"
    );
}

#[test]
fn finished_tasks_tell_the_session_agent_to_offer_a_review() {
    use super::view::of;
    use crate::cli::out::toon::encode;

    let mut up = live(&ClusterStatus::default(), &[]);
    up.review = 2;
    let h = home(State::Up(up));
    let hook = render(&h, Mode::Hook);
    assert!(
        hook.contains("review: 2 finished tasks wait for the user; offer to review them (idlemax review --json)"),
        "{hook}"
    );
    let (node, _) = of(&h, Mode::Hook);
    let toon = encode(&node);
    assert!(toon.contains("review_waiting: 2"), "{toon}");
    assert!(
        toon.contains("offer to review them (idlemax review --json)"),
        "{toon}"
    );
    let full = render(&h, Mode::Full);
    assert!(full.contains("review: 2 finished tasks wait"), "{full}");
    let (_, hints) = of(&h, Mode::Full);
    assert_eq!(
        hints[0],
        "Run `idlemax review` to see the 2 finished tasks, or ask your coding agent to review them"
    );
    // None waiting: the hook stays as small as before.
    let quiet = render(
        &home(State::Up(live(&ClusterStatus::default(), &[]))),
        Mode::Hook,
    );
    assert!(!quiet.contains("review"), "{quiet}");
}
