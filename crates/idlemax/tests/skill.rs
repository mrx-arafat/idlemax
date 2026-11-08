//! `idlemax skill` prints the agent skill and `--check` keeps the committed copy from drifting.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn skill(args: &[&str]) -> (i32, String) {
    let out = Command::new(BIN)
        .arg("skill")
        .args(args)
        .current_dir(repo_root())
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn the_committed_skill_is_current_and_a_stale_or_missing_one_fails_with_the_fix() {
    // The file in the repository is exactly what the binary generates.
    let (code, printed) = skill(&[]);
    assert_eq!(code, 0);
    let committed = std::fs::read_to_string(repo_root().join("skills/idlemax/SKILL.md")).unwrap();
    assert_eq!(
        printed, committed,
        "run `idlemax skill > skills/idlemax/SKILL.md`"
    );
    assert_eq!(
        skill(&["--check"]),
        (0, "skill: skills/idlemax/SKILL.md is current\n".into())
    );

    let tmp = tempfile::tempdir().unwrap();
    let copy = tmp.path().join("SKILL.md");
    let path = copy.to_str().unwrap();
    std::fs::write(&copy, &printed).unwrap();
    assert_eq!(skill(&["--check", path]).0, 0);

    std::fs::write(&copy, printed.replace("idlemax quota", "idlemax quotas")).unwrap();
    let (code, text) = skill(&["--check", path]);
    assert_eq!(code, 1);
    assert!(text.contains("is out of date"), "{text}");
    assert!(
        text.contains("`idlemax quotas`") && text.contains("help: Run `idlemax skill > "),
        "{text}"
    );

    std::fs::remove_file(&copy).unwrap();
    let (code, text) = skill(&["--check", path]);
    assert_eq!(code, 1);
    assert!(
        text.contains("does not exist") && text.contains("help: Run `idlemax skill > "),
        "{text}"
    );
}
