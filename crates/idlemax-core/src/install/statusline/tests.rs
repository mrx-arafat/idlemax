use super::*;

const W: Wrapper = Wrapper {
    script: "/home/u/.idlemax/bin/claude-statusline-capture.sh",
    idlemax_home: None,
};

/// Settings files as users really have them: odd indentation, key order, unicode, escapes,
/// a trailing newline or none, and a statusLine anywhere among the keys.
const FIXTURES: &[&str] = &[
    "{\n  \"model\": \"opus\",\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"~/.claude/statusline.sh\",\n    \"padding\": 0\n  },\n  \"env\": {\"A\": \"é\\u00e9 \\\"q\\\"\"}\n}\n",
    "{\"statusLine\":{\"type\":\"command\",\"command\":\"input=$(cat); echo \\\"$input\\\" | jq -r '.model'\"}}",
    "{\n\t\"z\": [1, 2],\n\t\"statusLine\": {\"type\": \"command\", \"command\": \"it's here\"}\n}",
    "{\n    \"a\": 1,\n    \"b\": {\"c\": [true, null]}\n}\n",
    "{\n  \"a\": 1\n}",
    "{}\n",
];

#[test]
fn install_then_uninstall_restores_every_byte() {
    for fixture in FIXTURES {
        let installed = plan_install(Some(fixture), &W).unwrap();
        let wrapped = inspect(Some(&installed)).unwrap();
        assert!(matches!(wrapped, Wiring::Wrapped(_)), "{fixture}");
        let restored = plan_uninstall(&installed, None).unwrap();
        if *fixture == "{}\n" {
            assert_eq!(restored, "{}\n");
        } else {
            assert_eq!(&restored, fixture, "{fixture}");
        }
    }
}

#[test]
fn install_is_idempotent_and_keeps_inline_shell_working() {
    let first = plan_install(Some(FIXTURES[1]), &W).unwrap();
    assert_eq!(plan_install(Some(&first), &W).unwrap(), first);
    let Wiring::Wrapped(w) = inspect(Some(&first)).unwrap() else {
        panic!("not wrapped");
    };
    assert_eq!(
        w.original.as_deref(),
        Some("input=$(cat); echo \"$input\" | jq -r '.model'")
    );
    // Uninstall leaves a file that was never wrapped alone.
    assert_eq!(plan_uninstall(FIXTURES[0], None).unwrap(), FIXTURES[0]);
}

#[test]
fn missing_file_gets_a_new_file_and_a_custom_home_is_passed_on() {
    let w = Wrapper {
        idlemax_home: Some("/tmp/a b/state"),
        ..W
    };
    let text = plan_install(None, &w).unwrap();
    let Wiring::Wrapped(found) = inspect(Some(&text)).unwrap() else {
        panic!("not wrapped");
    };
    assert_eq!(found.idlemax_home.as_deref(), Some("/tmp/a b/state"));
    assert_eq!(found.original, None);
    assert_eq!(plan_uninstall(&text, None).unwrap(), "{}\n");
}

#[test]
fn the_manual_readme_form_counts_as_installed_and_unwraps() {
    let manual = "{\"statusLine\": {\"type\": \"command\", \"command\": \"/repo/packaging/claude-statusline-capture.sh bash ~/s.sh\"}}";
    assert_eq!(plan_install(Some(manual), &W).unwrap(), manual);
    assert_eq!(
        plan_uninstall(manual, None).unwrap(),
        "{\"statusLine\": {\"type\": \"command\", \"command\": \"bash ~/s.sh\"}}"
    );
}

#[test]
fn unexpected_shapes_are_refused_with_a_reason() {
    let cases = [
        ("[1]", "expected a JSON object"),
        ("{\"statusLine\": \"x\"}", "expected an object"),
        (
            "{\"statusLine\": {\"type\": \"text\", \"command\": \"x\"}}",
            "not \"command\"",
        ),
        (
            "{\"statusLine\": {\"type\": \"command\"}}",
            "no \"command\" string",
        ),
        ("{\"statusLine\": {}, \"statusLine\": {}}", "more than one"),
        ("{\"a\": 1,}", "not valid JSON"),
    ];
    for (text, why) in cases {
        let err = plan_install(Some(text), &W).unwrap_err().to_string();
        assert!(err.contains(why), "{text}: {err}");
    }
}

#[test]
fn the_backup_restores_every_byte_only_while_the_file_is_untouched() {
    let original = "{\n  \"a\": 1,\n  \"statusLine\": {\"type\": \"command\", \"command\": \"x \\u00e9 \\/ y\"}\n}\n";
    let installed = plan_install(Some(original), &W).unwrap();
    // Without the backup an escape like \u00e9 comes back as the plain character.
    assert_ne!(plan_uninstall(&installed, None).unwrap(), original);
    assert_eq!(
        plan_uninstall(&installed, Some(original)).unwrap(),
        original
    );

    let edited = installed.replacen("\"a\": 1", "\"a\": 2", 1);
    let restored = plan_uninstall(&edited, Some(original)).unwrap();
    assert!(
        restored.contains("\"a\": 2") && !restored.contains("sh -c"),
        "{restored}"
    );
    // A backup that is itself wrapped is never mistaken for the original.
    assert_eq!(
        plan_uninstall(&installed, Some(&installed)).unwrap(),
        plan_uninstall(&installed, None).unwrap()
    );
}
