//! Adapter: seam for executing child processes with timeouts, throttles and clean groups.

use std::path::PathBuf;
use std::time::Duration;

use idlemax_core::job_run::Wrapper;

const MAX_OUTPUT_BYTES: u64 = 1024 * 1024; // 1 MiB cap per stream

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub stdin: Option<String>,
    pub limit: Duration,
    pub throttle: bool,
    pub tag: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CmdError {
    #[error("command timed out")]
    TimedOut,
    #[error("command execution failed: {0}")]
    Io(String),
}

pub trait Commands: Send + Sync {
    fn run(&self, cmd: &Cmd) -> Result<Output, CmdError>;
}

/// Pure helper: builds child environment from parent vars, inherits rule, prompt, and tag.
pub fn build_env<'a, I>(
    parent_vars: I,
    custom_env: &[(String, String)],
    tag: Option<u64>,
) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let mut out: Vec<(String, String)> = parent_vars
        .into_iter()
        .filter(|(k, _)| idlemax_core::job_run::inherits(k))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    out.push(("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()));
    if let Some(tag) = tag {
        out.push(("IDLEMAX_JOB_TAG".to_string(), tag.to_string()));
    }
    for (k, v) in custom_env {
        out.push((k.clone(), v.clone()));
    }
    out
}

pub struct OsCommands {
    wrapper: Wrapper,
    mem_mb: u64,
}

impl Default for OsCommands {
    fn default() -> Self {
        Self::new()
    }
}

impl OsCommands {
    pub fn new() -> Self {
        Self::with_options(
            crate::agent::runner::detect_wrapper(),
            idlemax_core::types::DEFAULT_MEM_MB,
        )
    }

    pub fn with_options(wrapper: Wrapper, mem_mb: u64) -> Self {
        Self { wrapper, mem_mb }
    }

    #[allow(dead_code)]
    pub fn with_wrapper(wrapper: Wrapper) -> Self {
        Self::with_options(wrapper, idlemax_core::types::DEFAULT_MEM_MB)
    }
}

fn build_command(cmd: &Cmd, wrapper: &Wrapper, mem_mb: u64) -> std::process::Command {
    let (prog, args) = if cmd.throttle {
        let mut argv = vec![cmd.program.clone()];
        argv.extend(cmd.args.iter().cloned());
        let unit = format!("idlemax-worker-{}", cmd.tag.unwrap_or(0));
        let wrapped = wrapper.wrap(&argv, mem_mb, &unit);
        if let Some((first, rest)) = wrapped.split_first() {
            (first.clone(), rest.to_vec())
        } else {
            (cmd.program.clone(), cmd.args.clone())
        }
    } else {
        (cmd.program.clone(), cmd.args.clone())
    };

    let mut command = std::process::Command::new(&prog);
    command.args(&args);

    #[cfg(unix)]
    {
        command.env_clear();
        let sys_vars: Vec<(String, String)> = std::env::vars().collect();
        let env_pairs = build_env(
            sys_vars.iter().map(|(k, v)| (k.as_str(), v.as_str())),
            &cmd.env,
            cmd.tag,
        );
        for (k, v) in env_pairs {
            command.env(k, v);
        }
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    {
        for (k, v) in &cmd.env {
            command.env(k, v);
        }
    }

    if let Some(ref cwd) = cmd.cwd {
        command.current_dir(cwd);
    }

    if cmd.stdin.is_some() {
        command.stdin(std::process::Stdio::piped());
    } else {
        command.stdin(std::process::Stdio::null());
    }
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    command
}

fn feed_stdin(child: &mut std::process::Child, input: Option<&str>) {
    let Some(input) = input else { return };
    if let Some(mut stdin) = child.stdin.take() {
        let input = input.to_string();
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = stdin.write_all(input.as_bytes());
        });
    }
}

fn drain_capped<R: std::io::Read + Send + 'static>(
    reader: Option<R>,
) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let s = if let Some(r) = reader {
            let mut buf = Vec::new();
            let mut take = r.take(MAX_OUTPUT_BYTES);
            let _ = take.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        } else {
            String::new()
        };
        let _ = tx.send(s);
    });
    rx
}

fn wait_with_limit(mut child: std::process::Child, limit: Duration) -> Result<Output, CmdError> {
    let child_id = child.id();
    let out_rx = drain_capped(child.stdout.take());
    let err_rx = drain_capped(child.stderr.take());

    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait().map_err(|e| CmdError::Io(e.to_string()))? {
            Some(s) => break s,
            None => {
                if start.elapsed() >= limit {
                    #[cfg(unix)]
                    {
                        let raw_pid = child_id as i32;
                        let pgid = nix::unistd::Pid::from_raw(raw_pid);
                        let _ = nix::sys::signal::killpg(pgid, nix::sys::signal::Signal::SIGKILL);
                        let _ = child.wait();
                        let deadline = std::time::Instant::now() + Duration::from_millis(500);
                        while std::time::Instant::now() < deadline {
                            if nix::sys::signal::killpg(pgid, None).is_err() {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(5));
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    return Err(CmdError::TimedOut);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };

    let stdout = out_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap_or_default();
    let stderr = err_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap_or_default();
    Ok(Output {
        code: status.code(),
        stdout,
        stderr,
    })
}

impl Commands for OsCommands {
    fn run(&self, cmd: &Cmd) -> Result<Output, CmdError> {
        let mut command = build_command(cmd, &self.wrapper, self.mem_mb);
        let mut child = command.spawn().map_err(|e| CmdError::Io(e.to_string()))?;
        feed_stdin(&mut child, cmd.stdin.as_deref());
        wait_with_limit(child, cmd.limit)
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct FakeCommands {
    pub calls: std::sync::Mutex<Vec<Cmd>>,
    pub responses: std::sync::Mutex<std::collections::VecDeque<Result<Output, CmdError>>>,
    #[allow(clippy::type_complexity)]
    pub fallback:
        std::sync::Mutex<Option<Box<dyn Fn(&Cmd) -> Result<Output, CmdError> + Send + Sync>>>,
}

#[cfg(test)]
impl FakeCommands {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_output(
        &self,
        code: Option<i32>,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
    ) {
        self.responses.lock().unwrap().push_back(Ok(Output {
            code,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }));
    }

    pub fn push_error(&self, err: CmdError) {
        self.responses.lock().unwrap().push_back(Err(err));
    }

    #[allow(dead_code)]
    pub fn set_fallback<F>(&self, f: F)
    where
        F: Fn(&Cmd) -> Result<Output, CmdError> + Send + Sync + 'static,
    {
        *self.fallback.lock().unwrap() = Some(Box::new(f));
    }

    pub fn calls(&self) -> Vec<Cmd> {
        self.calls.lock().unwrap().clone()
    }
}

#[cfg(test)]
impl Commands for FakeCommands {
    fn run(&self, cmd: &Cmd) -> Result<Output, CmdError> {
        self.calls.lock().unwrap().push(cmd.clone());
        if let Some(resp) = self.responses.lock().unwrap().pop_front() {
            return resp;
        }
        if let Some(ref f) = *self.fallback.lock().unwrap() {
            return f(cmd);
        }
        Ok(Output {
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_commands_times_out_and_leaves_no_process() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("pgid.txt");
        let pid_file_str = pid_file.display().to_string();

        let os = OsCommands::new();
        let script = format!("echo $$ > '{pid_file_str}'; sleep 30 & sleep 30");
        let cmd = Cmd {
            program: "sh".into(),
            args: vec!["-c".into(), script],
            cwd: None,
            env: Vec::new(),
            stdin: None,
            limit: Duration::from_millis(50),
            throttle: false,
            tag: None,
        };

        let start = std::time::Instant::now();
        let res = os.run(&cmd);
        let elapsed = start.elapsed();

        assert_eq!(res, Err(CmdError::TimedOut));
        assert!(
            elapsed < Duration::from_secs(1),
            "timed out too slowly: {elapsed:?}"
        );

        if let Ok(content) = std::fs::read_to_string(&pid_file) {
            if let Ok(pgid) = content.trim().parse::<i32>() {
                let check = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pgid), None);
                assert!(check.is_err(), "processes of group {pgid} are still alive!");
            }
        }
    }

    #[test]
    fn build_env_filters_secrets_and_keeps_only_minimal_env() {
        let parent_vars = [
            ("IDLEMAX_TOKEN", "super-secret-token"),
            ("AWS_SECRET_ACCESS_KEY", "aws-secret"),
            ("DATABASE_URL", "postgres://user:pass@localhost/db"),
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/home/test"),
            ("USER", "testuser"),
            ("LANG", "en_US.UTF-8"),
        ];

        let env_pairs = build_env(
            parent_vars,
            &[("CUSTOM_PAIR".into(), "custom_val".into())],
            Some(42),
        );

        assert!(!env_pairs.iter().any(|(k, _)| k == "IDLEMAX_TOKEN"));
        assert!(!env_pairs.iter().any(|(k, _)| k == "AWS_SECRET_ACCESS_KEY"));
        assert!(!env_pairs.iter().any(|(k, _)| k == "DATABASE_URL"));

        assert!(env_pairs
            .iter()
            .any(|(k, v)| k == "GIT_TERMINAL_PROMPT" && v == "0"));
        assert!(env_pairs
            .iter()
            .any(|(k, v)| k == "IDLEMAX_JOB_TAG" && v == "42"));
        assert!(env_pairs
            .iter()
            .any(|(k, v)| k == "CUSTOM_PAIR" && v == "custom_val"));
        assert!(env_pairs
            .iter()
            .any(|(k, v)| k == "PATH" && v == "/usr/bin:/bin"));
        assert!(env_pairs
            .iter()
            .any(|(k, v)| k == "HOME" && v == "/home/test"));
    }

    #[test]
    fn drain_capped_handles_non_utf8_lossy() {
        let bytes: &[u8] = &[0x66, 0x6f, 0x6f, 0xff, 0xfe, 0x62, 0x61, 0x72];
        let rx = drain_capped(Some(std::io::Cursor::new(bytes)));
        let out = rx.recv().unwrap();
        assert!(out.contains("foo"));
        assert!(out.contains("bar"));
    }

    #[test]
    fn fake_commands_records_calls_and_returns_scripted_outputs() {
        let fake = FakeCommands::new();
        fake.push_output(Some(0), "first output", "");
        fake.push_error(CmdError::TimedOut);

        let cmd1 = Cmd {
            program: "git".into(),
            args: vec!["status".into()],
            cwd: None,
            env: Vec::new(),
            stdin: None,
            limit: Duration::from_secs(10),
            throttle: false,
            tag: None,
        };
        let res1 = fake.run(&cmd1).unwrap();
        assert_eq!(res1.stdout, "first output");

        let res2 = fake.run(&cmd1);
        assert_eq!(res2, Err(CmdError::TimedOut));

        let calls = fake.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].program, "git");

        fake.set_fallback(|c| {
            Ok(Output {
                code: Some(0),
                stdout: format!("fallback: {}", c.program),
                stderr: String::new(),
            })
        });
        let res3 = fake.run(&cmd1).unwrap();
        assert_eq!(res3.stdout, "fallback: git");
    }

    #[test]
    fn os_commands_with_direct_wrapper() {
        let os = OsCommands::with_options(Wrapper::Direct, 256);
        let cmd = Cmd {
            program: "true".into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            stdin: None,
            limit: Duration::from_secs(5),
            throttle: true,
            tag: None,
        };
        let out = os.run(&cmd).expect("true failed");
        assert_eq!(out.code, Some(0));
    }
}
