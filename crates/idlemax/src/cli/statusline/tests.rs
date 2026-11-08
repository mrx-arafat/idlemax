use super::*;

fn places(root: &Path) -> Places {
    Places {
        settings: root.join("claude/settings.json"),
        wrapper: root.join("state/bin").join(WRAPPER_NAME),
        state: root.join("state"),
        custom_home: Some(root.join("state").to_string_lossy().into_owned()),
    }
}

const SETTINGS: &str = "{\n  \"theme\": \"dark\",\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"~/.claude/statusline.sh\"\n  }\n}\n";

#[test]
fn install_then_uninstall_round_trips_a_real_file_through_a_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let p = places(dir.path());
    let real = dir.path().join("dotfiles/settings.json");
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    std::fs::create_dir_all(p.settings.parent().unwrap()).unwrap();
    std::fs::write(&real, SETTINGS).unwrap();
    std::os::unix::fs::symlink(&real, &p.settings).unwrap();

    let dry = install(&p, true).unwrap();
    assert!(dry.changed && dry.diff.contains("+    \"command\"") && !p.wrapper.exists());
    assert_eq!(std::fs::read_to_string(&real).unwrap(), SETTINGS);

    let done = install(&p, false).unwrap();
    assert!(done.changed && p.wrapper.exists());
    assert!(p.settings.is_symlink(), "the symlink must survive");
    assert_eq!(
        std::fs::read_to_string(done.backup.unwrap()).unwrap(),
        SETTINGS
    );
    let again = install(&p, false).unwrap();
    assert!(!again.changed);

    let state = status(&p).unwrap();
    assert_eq!(
        (state.state, state.wrapper_present),
        ("installed", Some(true))
    );
    assert_eq!(
        state.original_command.as_deref(),
        Some("~/.claude/statusline.sh")
    );
    assert_eq!(state.capture_age_secs, None);

    uninstall(&p, false).unwrap();
    assert_eq!(std::fs::read_to_string(&real).unwrap(), SETTINGS);
    assert_eq!(status(&p).unwrap().state, "not_wrapped");
    assert!(!uninstall(&p, false).unwrap().changed);
}

#[test]
fn an_unexpected_shape_is_refused_and_the_file_is_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let p = places(dir.path());
    std::fs::create_dir_all(p.settings.parent().unwrap()).unwrap();
    std::fs::write(&p.settings, "{\"statusLine\": \"plain string\"}").unwrap();
    let err = format!("{:#}", install(&p, false).err().unwrap());
    assert!(
        err.contains("settings.json: \"statusLine\" is a string"),
        "{err}"
    );
    assert!(err.contains("edit it by hand"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&p.settings).unwrap(),
        "{\"statusLine\": \"plain string\"}"
    );
    assert!(!p.wrapper.exists());
}

#[cfg(unix)]
#[test]
fn install_creates_a_missing_state_dir_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let p = places(dir.path());
    install(&p, false).unwrap();
    for made in [p.state.clone(), p.wrapper.parent().unwrap().to_path_buf()] {
        let mode = std::fs::metadata(&made).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{}", made.display());
    }
}
