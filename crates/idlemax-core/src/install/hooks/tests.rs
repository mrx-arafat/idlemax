use std::path::Path;

use serde_json::Value;

use super::*;

const CMD: &str = "idlemax hooks summary";

/// Every layout a settings file can plausibly have, each valid JSON with no hook of ours.
const SHAPES: &[&str] = &[
    "{}",
    "{}\n",
    "{\"model\":\"opus\"}",
    "{\n  \"model\": \"opus\",\n  \"permissions\": {\"allow\": [\"Bash(ls)\"]}\n}\n",
    "{\n    \"env\": {\"A\": \"1\"}\n}",
    "{\n  \"hooks\": {\n    \"PreToolUse\": [\n      {\"matcher\": \"Bash\", \"hooks\": [{\"type\": \"command\", \"command\": \"echo\"}]}\n    ]\n  }\n}\n",
    "{\"hooks\":{\"SessionStart\":[{\"matcher\":\"\",\"hooks\":[{\"type\":\"command\",\"command\":\"gh-axi\"}]}]}}",
    "{\n  \"hooks\": {\n    \"SessionStart\": [\n      {\n        \"matcher\": \"\",\n        \"hooks\": [\n          {\"type\": \"command\", \"command\": \"gh-axi\"}\n        ]\n      }\n    ]\n  },\n  \"theme\": \"dark\"\n}\n",
];

fn commands(text: &str) -> Vec<String> {
    let v: Value = serde_json::from_str(text).unwrap();
    v["hooks"][EVENT]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|g| g["hooks"].as_array().into_iter().flatten())
        .filter_map(|h| h["command"].as_str().map(String::from))
        .collect()
}

#[test]
fn a_new_file_is_pinned_exactly() {
    let text = plan_install(None, CMD).unwrap();
    assert_eq!(
        text,
        "{\n  \"hooks\": {\n    \"SessionStart\": [\n      {\n        \"matcher\": \"\",\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \"idlemax hooks summary\",\n            \"timeout\": 10\n          }\n        ]\n      }\n    ]\n  }\n}\n"
    );
    assert_eq!(
        inspect(Some(&text)).unwrap(),
        HookState::Installed {
            command: CMD.into()
        }
    );
    assert_eq!(inspect(None).unwrap(), HookState::NoFile);
}

#[test]
fn install_adds_one_hook_to_every_shape_and_uninstall_restores_the_bytes() {
    for before in SHAPES {
        assert_eq!(
            inspect(Some(before)).unwrap(),
            HookState::NotInstalled,
            "{before}"
        );
        let after = plan_install(Some(before), CMD).unwrap();
        assert_ne!(&after, before);
        let ours: Vec<_> = commands(&after)
            .into_iter()
            .filter(|c| is_ours(c))
            .collect();
        assert_eq!(ours, [CMD], "one hook of ours in {after}");
        // Others' hooks and keys are untouched.
        let mut others = commands(before);
        others.push(CMD.into());
        assert_eq!(commands(&after), others, "{after}");
        // Running it again changes nothing.
        assert_eq!(plan_install(Some(&after), CMD).unwrap(), after);
        // Structural removal, with no backup, gives the original bytes back.
        assert_eq!(&plan_uninstall(&after, None).unwrap(), before, "{after}");
    }
    // Nothing to remove is a no-op.
    assert_eq!(plan_uninstall("{}", None).unwrap(), "{}");
    // A container the user left empty is pruned with our hook, since it holds nothing.
    for empty in [
        "{\n  \"hooks\": {}\n}\n",
        "{\n  \"hooks\": {\"SessionStart\": []}\n}\n",
    ] {
        let installed = plan_install(Some(empty), CMD).unwrap();
        assert_eq!(commands(&installed), [CMD], "{installed}");
        assert_eq!(plan_uninstall(&installed, None).unwrap(), "{}\n", "{empty}");
    }
}

#[test]
fn a_moved_binary_is_repaired_in_place_and_nothing_else_changes() {
    let before = "{\n  \"theme\": \"dark\"\n}\n";
    let old = plan_install(Some(before), "/old/idlemax hooks summary").unwrap();
    let new = plan_install(Some(&old), "/new/bin/idlemax hooks summary").unwrap();
    assert_eq!(new, old.replace("/old/idlemax", "/new/bin/idlemax"));
    assert_eq!(
        inspect(Some(&new)).unwrap(),
        HookState::Installed {
            command: "/new/bin/idlemax hooks summary".into()
        }
    );
}

#[test]
fn uninstall_keeps_the_users_other_hooks_and_prefers_the_backup() {
    // Ours shares a group with the user's own hook: only ours goes.
    let shared = "{\"hooks\": {\"SessionStart\": [{\"matcher\": \"\", \"hooks\": [\n  {\"type\": \"command\", \"command\": \"mine\"},\n  {\"type\": \"command\", \"command\": \"idlemax hooks summary\"}\n]}]}}";
    let out = plan_uninstall(shared, None).unwrap();
    assert_eq!(commands(&out), ["mine"]);

    // Edited since install: the backup is not "before plus our edit", so it is not used.
    let before = "{\n  \"a\": 1\n}\n";
    let installed = plan_install(Some(before), CMD).unwrap();
    let edited = installed.replace("\"a\": 1", "\"a\": 2");
    assert_eq!(
        plan_uninstall(&edited, Some(before)).unwrap(),
        "{\n  \"a\": 2\n}\n"
    );
    // Untouched since install: the backup comes back byte for byte.
    assert_eq!(plan_uninstall(&installed, Some(before)).unwrap(), before);
}

#[test]
fn shapes_it_does_not_understand_are_refused_not_guessed() {
    let bad = [
        ("[1]", "top level"),
        ("{\"hooks\": []}", "\"hooks\" is an array"),
        (
            "{\"hooks\": {\"SessionStart\": {}}}",
            "\"hooks.SessionStart\" is an object",
        ),
        ("{\"hooks\": {\"SessionStart\": [3]}}", "expected an object"),
        (
            "{\"hooks\": {\"SessionStart\": [{\"hooks\": \"x\"}]}}",
            "expected an array of objects",
        ),
        ("{\"hooks\": {}, \"hooks\": {}}", "more than one \"hooks\""),
    ];
    for (text, why) in bad {
        let err = plan_install(Some(text), CMD).unwrap_err().to_string();
        assert!(err.contains(why), "{text}: {err}");
        assert!(plan_uninstall(text, None).is_err(), "{text}");
    }
    assert!(matches!(
        plan_install(Some("{"), CMD),
        Err(HooksError::Invalid { format: "JSON", .. })
    ));
}

#[test]
fn our_command_is_recognised_by_program_name_and_subcommand_only() {
    for yes in [
        CMD,
        "/usr/local/bin/idlemax hooks summary",
        "'/Users/me/my bin/idlemax' hooks summary",
    ] {
        assert!(is_ours(yes), "{yes}");
    }
    for no in [
        "idlemax status",
        "other hooks summary",
        "idlemax hooks summary --json",
        "",
        "gh-axi",
    ] {
        assert!(!is_ours(no), "{no}");
    }
    assert_eq!(
        hook_command("/Users/me/my bin/idlemax"),
        "'/Users/me/my bin/idlemax' hooks summary"
    );
    assert!(is_ours(&hook_command("/Users/me/my bin/idlemax")));

    let exe = Path::new("/home/me/.local/bin/idlemax");
    assert_eq!(program_for(exe, Some(exe)), "idlemax");
    assert_eq!(
        program_for(exe, Some(Path::new("/usr/bin/idlemax"))),
        "/home/me/.local/bin/idlemax"
    );
    assert_eq!(program_for(exe, None), "/home/me/.local/bin/idlemax");
}
