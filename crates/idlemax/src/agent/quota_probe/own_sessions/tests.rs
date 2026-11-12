//! Tests for own_sessions worker exclusion.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use idlemax_core::types::UnixMs;

use super::{interactive_use, OwnSessions, OWN_SESSION_WALK};
use crate::agent::quota_probe::ProbeDirs;

fn write_session_with_cwd(
    dir: &Path,
    name: &str,
    cwd: &Path,
    mtime_secs_ago: u64,
) -> (PathBuf, UnixMs) {
    let file = dir.join(name);
    let cwd_escaped = cwd.to_str().unwrap().replace('\\', "\\\\");
    let line = format!(
        r#"{{"type":"user","cwd":"{}","sessionId":"s1","message":{{"role":"user","content":"x"}}}}"#,
        cwd_escaped
    );
    fs::write(&file, format!("{line}\n")).unwrap();
    let mtime = SystemTime::now() - Duration::from_secs(mtime_secs_ago);
    File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    let unix_ms = mtime
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as UnixMs;
    (file, unix_ms)
}

#[test]
fn interactive_use_skips_worker_session_newer_than_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let claude_root = tmp.path().join("claude");
    let proj_dir = claude_root.join("projects").join("p1");
    fs::create_dir_all(&proj_dir).unwrap();
    let worktrees_dir = tmp.path().join("worktrees");
    fs::create_dir_all(&worktrees_dir).unwrap();

    let owner_cwd = tmp.path().join("owner_repo");
    let worker_cwd = worktrees_dir.join("repo").join("task-1");

    let (_owner_file, owner_mtime) =
        write_session_with_cwd(&proj_dir, "owner.jsonl", &owner_cwd, 200);
    let (_worker_file, _worker_mtime) =
        write_session_with_cwd(&proj_dir, "worker.jsonl", &worker_cwd, 100);

    let dirs = ProbeDirs {
        state_dir: tmp.path().join("state"),
        claude_dir: Some(claude_root),
        codex_dir: None,
        agy_dir: None,
        worktrees_dir: Some(worktrees_dir),
    };

    let mut own = OwnSessions::new();
    let active = interactive_use(&dirs, &mut own, UnixMs::MAX);
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].0, "claude");
    assert_eq!(active[0].1, owner_mtime);
}

#[test]
fn interactive_use_absent_with_only_worker_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let claude_root = tmp.path().join("claude");
    let proj_dir = claude_root.join("projects").join("p1");
    fs::create_dir_all(&proj_dir).unwrap();
    let worktrees_dir = tmp.path().join("worktrees");
    fs::create_dir_all(&worktrees_dir).unwrap();

    let worker_cwd = worktrees_dir.join("repo").join("task-1");
    write_session_with_cwd(&proj_dir, "worker.jsonl", &worker_cwd, 100);

    let dirs = ProbeDirs {
        state_dir: tmp.path().join("state"),
        claude_dir: Some(claude_root),
        codex_dir: None,
        agy_dir: None,
        worktrees_dir: Some(worktrees_dir),
    };

    let mut own = OwnSessions::new();
    let active = interactive_use(&dirs, &mut own, UnixMs::MAX);
    assert!(
        active.iter().all(|(cli, _)| cli != "claude"),
        "claude must be absent when all sessions are worker sessions"
    );
}

#[test]
fn interactive_use_reports_owner_with_six_worker_sessions_newer() {
    let tmp = tempfile::tempdir().unwrap();
    let claude_root = tmp.path().join("claude");
    let proj_dir = claude_root.join("projects").join("p1");
    fs::create_dir_all(&proj_dir).unwrap();
    let worktrees_dir = tmp.path().join("worktrees");
    fs::create_dir_all(&worktrees_dir).unwrap();

    let owner_cwd = tmp.path().join("owner_repo");
    let worker_cwd = worktrees_dir.join("repo").join("task-1");

    let (_owner_file, owner_mtime) =
        write_session_with_cwd(&proj_dir, "owner.jsonl", &owner_cwd, 1000);

    for i in 1..=6 {
        write_session_with_cwd(
            &proj_dir,
            &format!("worker_{i}.jsonl"),
            &worker_cwd,
            100 + i * 10,
        );
    }

    let dirs = ProbeDirs {
        state_dir: tmp.path().join("state"),
        claude_dir: Some(claude_root),
        codex_dir: None,
        agy_dir: None,
        worktrees_dir: Some(worktrees_dir),
    };

    let mut own = OwnSessions::new();
    let active = interactive_use(&dirs, &mut own, UnixMs::MAX);
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].0, "claude");
    assert_eq!(active[0].1, owner_mtime);
}

#[test]
fn interactive_use_reports_newest_when_worker_sessions_exceed_walk_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let claude_root = tmp.path().join("claude");
    let proj_dir = claude_root.join("projects").join("p1");
    fs::create_dir_all(&proj_dir).unwrap();
    let worktrees_dir = tmp.path().join("worktrees");
    fs::create_dir_all(&worktrees_dir).unwrap();

    let worker_cwd = worktrees_dir.join("repo").join("task-1");

    let mut newest_time = 0;
    for i in 0..=(OWN_SESSION_WALK) {
        let secs_ago = 100 + (i as u64) * 5;
        let (_, t) = write_session_with_cwd(
            &proj_dir,
            &format!("worker_{i:03}.jsonl"),
            &worker_cwd,
            secs_ago,
        );
        if i == 0 {
            newest_time = t;
        }
    }

    let dirs = ProbeDirs {
        state_dir: tmp.path().join("state"),
        claude_dir: Some(claude_root),
        codex_dir: None,
        agy_dir: None,
        worktrees_dir: Some(worktrees_dir),
    };

    let mut own = OwnSessions::new();
    let active = interactive_use(&dirs, &mut own, UnixMs::MAX);
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].0, "claude");
    assert_eq!(active[0].1, newest_time);
}

#[test]
fn own_sessions_cache_avoids_rereading_classified_files() {
    let tmp = tempfile::tempdir().unwrap();
    let claude_root = tmp.path().join("claude");
    let proj_dir = claude_root.join("projects").join("p1");
    fs::create_dir_all(&proj_dir).unwrap();
    let worktrees_dir = tmp.path().join("worktrees");
    fs::create_dir_all(&worktrees_dir).unwrap();

    let owner_cwd = tmp.path().join("owner_repo");
    let worker_cwd = worktrees_dir.join("repo").join("task-1");

    let (_owner_file, owner_mtime) =
        write_session_with_cwd(&proj_dir, "owner.jsonl", &owner_cwd, 300);
    let (worker_file, _worker_mtime) =
        write_session_with_cwd(&proj_dir, "worker.jsonl", &worker_cwd, 100);

    let dirs = ProbeDirs {
        state_dir: tmp.path().join("state"),
        claude_dir: Some(claude_root),
        codex_dir: None,
        agy_dir: None,
        worktrees_dir: Some(worktrees_dir),
    };

    let mut own = OwnSessions::new();
    let active1 = interactive_use(&dirs, &mut own, UnixMs::MAX);
    assert_eq!(active1[0].1, owner_mtime);

    // Empty the worker file on disk so that reading it again would find no cwd
    fs::write(&worker_file, "").unwrap();

    // Second call: worker.jsonl is cached as worker session, so still skipped
    let active2 = interactive_use(&dirs, &mut own, UnixMs::MAX);
    assert_eq!(active2[0].1, owner_mtime);
}

#[test]
fn own_sessions_does_not_cache_without_cwd_record() {
    let tmp = tempfile::tempdir().unwrap();
    let claude_root = tmp.path().join("claude");
    let proj_dir = claude_root.join("projects").join("p1");
    fs::create_dir_all(&proj_dir).unwrap();
    let worktrees_dir = tmp.path().join("worktrees");
    fs::create_dir_all(&worktrees_dir).unwrap();

    let owner_cwd = tmp.path().join("owner_repo");
    let worker_cwd = worktrees_dir.join("repo").join("task-1");

    let (_owner_file, owner_mtime) =
        write_session_with_cwd(&proj_dir, "owner.jsonl", &owner_cwd, 300);

    // Newer file initially lacks a cwd record
    let worker_file = proj_dir.join("worker.jsonl");
    fs::write(
        &worker_file,
        "{\"type\":\"progress\",\"status\":\"starting\"}\n",
    )
    .unwrap();
    let mtime = SystemTime::now() - Duration::from_secs(100);
    File::options()
        .write(true)
        .open(&worker_file)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    let worker_mtime = mtime
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as UnixMs;

    let dirs = ProbeDirs {
        state_dir: tmp.path().join("state"),
        claude_dir: Some(claude_root),
        codex_dir: None,
        agy_dir: None,
        worktrees_dir: Some(worktrees_dir),
    };

    let mut own = OwnSessions::new();
    // First call: worker file lacks cwd, so it is not cached as non-worker;
    // since it is newer and not classified as worker, it is reported as interactive use.
    let active1 = interactive_use(&dirs, &mut own, UnixMs::MAX);
    assert_eq!(active1[0].1, worker_mtime);

    // Rewritten with real Claude transcript record containing worker cwd
    let cwd_escaped = worker_cwd.to_str().unwrap().replace('\\', "\\\\");
    fs::write(
        &worker_file,
        format!(
            "{{\"type\":\"user\",\"cwd\":\"{}\",\"sessionId\":\"s1\",\"message\":{{\"role\":\"user\",\"content\":\"x\"}}}}\n",
            cwd_escaped
        ),
    )
    .unwrap();
    File::options()
        .write(true)
        .open(&worker_file)
        .unwrap()
        .set_modified(mtime)
        .unwrap();

    // Second call: worker session is recognized and skipped; owner mtime reported.
    let active2 = interactive_use(&dirs, &mut own, UnixMs::MAX);
    assert_eq!(active2[0].1, owner_mtime);
}

#[test]
fn is_under_canonicalizes_relative_components() {
    let tmp = tempfile::tempdir().unwrap();
    let worktrees_dir = tmp.path().join("worktrees");
    let owner_repo = tmp.path().join("owner_repo");
    fs::create_dir_all(&worktrees_dir).unwrap();
    fs::create_dir_all(&owner_repo).unwrap();

    // Lexically starts with worktrees_dir if written as worktrees_dir/../owner_repo,
    // but canonicalizes outside worktrees_dir.
    let dot_dot_path = worktrees_dir.join("..").join("owner_repo");
    assert!(!super::is_under(&dot_dot_path, &worktrees_dir));

    let worker_dir = worktrees_dir.join("task-1");
    fs::create_dir_all(&worker_dir).unwrap();
    assert!(super::is_under(&worker_dir, &worktrees_dir));
}
