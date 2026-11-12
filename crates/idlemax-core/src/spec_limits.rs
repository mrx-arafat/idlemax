//! Domain: how big a job spec may be. The API takes specs from anyone holding the token and the
//! coordinator keeps every one of them, so each field is bounded; a spec that is over a limit is
//! a 400 that names the field and the limit. The sizes are far above anything real. Pure.

use crate::types::{JobSpec, SpecError};
use crate::worker::WorkerSpec;

pub const MAX_NAME_CHARS: usize = 200;
/// Arguments in `command`, the size of one, and of all together (Linux caps one at 128 KiB).
pub const MAX_ARGS: usize = 1024;
pub const MAX_ARG_BYTES: usize = 128 * 1024;
pub const MAX_COMMAND_BYTES: usize = 512 * 1024;
pub const MAX_ENV_VARS: usize = 256;
pub const MAX_ENV_NAME_BYTES: usize = 256;
pub const MAX_ENV_VALUE_BYTES: usize = 32 * 1024;
pub const MAX_ENV_TOTAL_BYTES: usize = 256 * 1024;
/// The advisor's AI prompt is about 45 KiB.
pub const MAX_STDIN_BYTES: usize = 1024 * 1024;
pub const MAX_CWD_BYTES: usize = 4096;
/// Entries in `requires` and `devices`, and the length of each; also `ai_account`.
pub const MAX_LIST_ENTRIES: usize = 64;
pub const MAX_LIST_ITEM_CHARS: usize = 128;
/// Maximum allowed runtime: 30 days.
pub const MAX_RUNTIME_SECS: usize = 30 * 86_400;

fn over(field: &'static str, limit: usize, unit: &'static str) -> SpecError {
    SpecError::OverLimit { field, limit, unit }
}

/// Checks every size and character rule of `spec`; the first breach is the error.
pub fn check(spec: &JobSpec) -> Result<(), SpecError> {
    if spec.name.chars().any(char::is_control) {
        return Err(SpecError::ControlChar("name"));
    }
    if spec.name.chars().count() > MAX_NAME_CHARS {
        return Err(over("name", MAX_NAME_CHARS, "characters"));
    }
    if spec.max_runtime_secs > MAX_RUNTIME_SECS as u64 {
        return Err(over("max_runtime_secs", MAX_RUNTIME_SECS, "seconds"));
    }
    command(&spec.command)?;
    env(spec)?;
    if spec
        .stdin
        .as_ref()
        .is_some_and(|s| s.len() > MAX_STDIN_BYTES)
    {
        return Err(over("stdin", MAX_STDIN_BYTES, "bytes"));
    }
    if let Some(cwd) = &spec.cwd {
        if cwd.contains('\0') {
            return Err(SpecError::Nul("cwd"));
        }
        if cwd.len() > MAX_CWD_BYTES {
            return Err(over("cwd", MAX_CWD_BYTES, "bytes"));
        }
    }
    for (field, list) in [("requires", &spec.requires), ("devices", &spec.devices)] {
        if list.len() > MAX_LIST_ENTRIES {
            return Err(over(field, MAX_LIST_ENTRIES, "entries"));
        }
        if list.iter().any(|e| e.chars().count() > MAX_LIST_ITEM_CHARS) {
            return Err(over(field, MAX_LIST_ITEM_CHARS, "characters per entry"));
        }
    }
    if let Some(w) = &spec.worker {
        worker(w)?;
    }
    match &spec.ai_account {
        Some(a) if a.chars().count() > MAX_LIST_ITEM_CHARS => {
            Err(over("ai_account", MAX_LIST_ITEM_CHARS, "characters"))
        }
        _ => Ok(()),
    }
}

/// The worker section's texts are bounded like `cwd`, its lists like `requires`, and the done
/// check like one command argument.
fn worker(w: &WorkerSpec) -> Result<(), SpecError> {
    let reviewer = w.reviewer.as_ref();
    let texts = [
        ("worker.project", Some(&w.project)),
        ("worker.task", Some(&w.task)),
        ("worker.task_title", Some(&w.task_title)),
        ("worker.repo", Some(&w.repo)),
        ("worker.base_branch", w.base_branch.as_ref()),
        ("worker.branch", w.branch.as_ref()),
        ("worker.worktree", w.worktree.as_ref()),
        ("worker.model", Some(&w.model)),
        ("worker.session", w.session.as_ref()),
        ("worker.reviewer.model", reviewer.map(|r| &r.model)),
        ("worker.reviewer.account", reviewer.map(|r| &r.account)),
    ];
    for (field, text) in texts {
        bounded(field, text.map_or("", String::as_str), MAX_CWD_BYTES)?;
    }
    bounded("worker.done_check", &w.done_check, MAX_ARG_BYTES)?;
    let lists = [
        ("worker.setup", &w.setup),
        ("worker.check", &w.check),
        ("worker.scope_paths", &w.scope_paths),
        ("worker.off_limits", &w.off_limits),
        ("worker.core_paths", &w.core_paths),
    ];
    for (field, list) in lists {
        if list.len() > MAX_LIST_ENTRIES {
            return Err(over(field, MAX_LIST_ENTRIES, "entries"));
        }
        for entry in list {
            bounded(field, entry, MAX_CWD_BYTES)?;
        }
    }
    Ok(())
}

fn bounded(field: &'static str, text: &str, limit: usize) -> Result<(), SpecError> {
    if text.contains('\0') {
        return Err(SpecError::Nul(field));
    }
    if text.len() > limit {
        return Err(over(field, limit, "bytes"));
    }
    Ok(())
}

fn command(argv: &[String]) -> Result<(), SpecError> {
    if argv.len() > MAX_ARGS {
        return Err(over("command", MAX_ARGS, "arguments"));
    }
    if argv.iter().any(|a| a.contains('\0')) {
        return Err(SpecError::Nul("command"));
    }
    if argv.iter().any(|a| a.len() > MAX_ARG_BYTES) {
        return Err(over("command argument", MAX_ARG_BYTES, "bytes"));
    }
    if argv.iter().map(String::len).sum::<usize>() > MAX_COMMAND_BYTES {
        return Err(over("command", MAX_COMMAND_BYTES, "bytes in all"));
    }
    Ok(())
}

fn env(spec: &JobSpec) -> Result<(), SpecError> {
    if spec.env.len() > MAX_ENV_VARS {
        return Err(over("env", MAX_ENV_VARS, "variables"));
    }
    for (name, value) in &spec.env {
        if name.is_empty() || name.contains(['=', '\0']) {
            return Err(SpecError::BadEnvName);
        }
        if name.len() > MAX_ENV_NAME_BYTES {
            return Err(over("env name", MAX_ENV_NAME_BYTES, "bytes"));
        }
        if value.contains('\0') {
            return Err(SpecError::Nul("env"));
        }
        if value.len() > MAX_ENV_VALUE_BYTES {
            return Err(over("env value", MAX_ENV_VALUE_BYTES, "bytes"));
        }
    }
    let total: usize = spec.env.iter().map(|(k, v)| k.len() + v.len()).sum();
    if total > MAX_ENV_TOTAL_BYTES {
        return Err(over("env", MAX_ENV_TOTAL_BYTES, "bytes in all"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> JobSpec {
        JobSpec {
            name: "n".into(),
            command: vec!["/bin/echo".into()],
            ..JobSpec::default()
        }
    }

    /// The rules the API test does not walk: NUL bytes, the totals and the short lists.
    #[test]
    fn nul_bytes_totals_and_lists_are_bounded() {
        assert_eq!(check(&spec()), Ok(()));
        let mut nul = spec();
        nul.command.push("a\0b".into());
        assert_eq!(check(&nul), Err(SpecError::Nul("command")));

        let mut cwd = spec();
        cwd.cwd = Some("/tmp\0x".into());
        assert_eq!(check(&cwd), Err(SpecError::Nul("cwd")));

        // Many arguments that are each fine but together over the total.
        let mut total = spec();
        total.command = vec!["a".repeat(MAX_ARG_BYTES); MAX_COMMAND_BYTES / MAX_ARG_BYTES + 1];
        assert_eq!(
            check(&total),
            Err(over("command", MAX_COMMAND_BYTES, "bytes in all"))
        );

        let mut env = spec();
        for i in 0..9 {
            env.env
                .insert(format!("K{i}"), "v".repeat(MAX_ENV_VALUE_BYTES));
        }
        assert_eq!(
            check(&env),
            Err(over("env", MAX_ENV_TOTAL_BYTES, "bytes in all"))
        );

        let mut devices = spec();
        devices.devices = vec!["d".into(); MAX_LIST_ENTRIES + 1];
        assert_eq!(
            check(&devices),
            Err(over("devices", MAX_LIST_ENTRIES, "entries"))
        );
        let mut account = spec();
        account.ai_account = Some("a".repeat(MAX_LIST_ITEM_CHARS + 1));
        assert!(check(&account).is_err());

        let mut long_job = spec();
        long_job.max_runtime_secs = MAX_RUNTIME_SECS as u64 + 1;
        assert_eq!(
            check(&long_job),
            Err(over("max_runtime_secs", MAX_RUNTIME_SECS, "seconds"))
        );
    }
}
