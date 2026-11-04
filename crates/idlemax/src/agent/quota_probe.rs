//! Adapter: reads AI CLI usage files (read-only) into quota snapshots and interactive-use times.
//!
//! Blocking file I/O: call from `spawn_blocking`. A probe walks about 2k file entries.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use idlemax_core::quota::{rolling_5h, window_from_minutes, AccountConfig, QuotaConfig};
use idlemax_core::types::{ProbeSource, QuotaSnapshot, QuotaWindowKind, UnixMs};
use idlemax_core::Paths;
use serde::Deserialize;

/// Codex session files checked, newest mtime first. Old files get their mtime bumped
/// without new events, so the newest event timestamp decides, not the newest mtime.
const CODEX_CANDIDATES: usize = 3;
const TAIL_START_BYTES: u64 = 64 * 1024;
const TAIL_MAX_BYTES: u64 = 2 * 1024 * 1024;
const WALK_MAX_DEPTH: usize = 6;

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{}: {source}", path.display())]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("no default {0} path (HOME unset); set probe_path")]
    NoDefaultPath(&'static str),
}

impl ProbeError {
    fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Self + '_ {
        move |source| ProbeError::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    fn json(path: &Path) -> impl FnOnce(serde_json::Error) -> Self + '_ {
        move |source| ProbeError::Json {
            path: path.to_path_buf(),
            source,
        }
    }

    fn is_not_found(&self) -> bool {
        matches!(self, ProbeError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
    }
}

/// Where the probes look. Built once at agent startup; tests point it at temp dirs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeDirs {
    /// `$IDLEMAX_HOME/agent`: default Claude capture and manual files.
    pub state_dir: PathBuf,
    /// `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
    pub claude_dir: Option<PathBuf>,
    /// `$CODEX_HOME`, else `~/.codex`.
    pub codex_dir: Option<PathBuf>,
}

impl ProbeDirs {
    pub fn from_env(paths: &Paths) -> Self {
        let home = env_dir(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
        Self {
            state_dir: paths.agent_state_dir(),
            claude_dir: env_dir("CLAUDE_CONFIG_DIR")
                .or_else(|| home.as_ref().map(|h| h.join(".claude"))),
            codex_dir: env_dir("CODEX_HOME").or_else(|| home.map(|h| h.join(".codex"))),
        }
    }
}

/// Set and non-empty only: an empty HOME must never resolve to some other directory.
fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Snapshots for every configured account whose probe data exists, minus windows that
/// already reset. Errors are logged; one broken account never hides the others.
pub fn probe(cfg: &QuotaConfig, dirs: &ProbeDirs, now_ms: UnixMs) -> Vec<QuotaSnapshot> {
    let mut out = Vec::new();
    for account in &cfg.accounts {
        match probe_account(account, dirs) {
            Ok(snaps) => out.extend(
                snaps
                    .into_iter()
                    .filter(|s| s.resets_at_ms.is_none_or(|r| r > now_ms)),
            ),
            Err(e) if e.is_not_found() => {
                tracing::debug!(account = %account.name, "quota probe: {e}")
            }
            Err(e) => tracing::warn!(account = %account.name, "quota probe: {e}"),
        }
    }
    out
}

pub fn probe_account(
    account: &AccountConfig,
    dirs: &ProbeDirs,
) -> Result<Vec<QuotaSnapshot>, ProbeError> {
    let source = account.effective_probe();
    let path = match &account.probe_path {
        Some(p) => PathBuf::from(p),
        None => default_path(source, &account.name, dirs)?,
    };
    match source {
        ProbeSource::ClaudeStatusline => read_claude_statusline(&path, &account.name),
        ProbeSource::CodexSessions => read_codex_sessions(&path, &account.name),
        ProbeSource::Manual => read_manual(&path, &account.name),
    }
}

fn default_path(
    source: ProbeSource,
    account: &str,
    dirs: &ProbeDirs,
) -> Result<PathBuf, ProbeError> {
    match source {
        ProbeSource::ClaudeStatusline => Ok(dirs.state_dir.join("claude-statusline.json")),
        ProbeSource::CodexSessions => dirs
            .codex_dir
            .as_ref()
            .map(|d| d.join("sessions"))
            .ok_or(ProbeError::NoDefaultPath("codex sessions")),
        ProbeSource::Manual => Ok(dirs.state_dir.join(format!("quota-{account}.json"))),
    }
}

/// (cli name, newest session file mtime) for Claude and Codex. idlemax's own AI jobs
/// count too, which only lengthens the hold.
// SHORTCUT: agy is not detected (its session dir is unverified), add it once located.
pub fn interactive_use(dirs: &ProbeDirs, now_ms: UnixMs) -> Vec<(String, UnixMs)> {
    let sources = [
        (
            "claude",
            dirs.claude_dir.as_ref().map(|d| d.join("projects")),
        ),
        ("codex", dirs.codex_dir.as_ref().map(|d| d.join("sessions"))),
    ];
    let mut out = Vec::new();
    for (cli, dir) in sources {
        let Some(dir) = dir else { continue };
        match jsonl_files(&dir) {
            Ok(files) => out.extend(
                files
                    .iter()
                    .map(|(t, _)| *t)
                    .max()
                    .map(|t| (cli.to_string(), t.min(now_ms))),
            ),
            Err(e) => tracing::debug!(cli, "interactive use: {e}"),
        }
    }
    out
}

/// Claude Code statusline JSON (the whole stdin object or just its `rate_limits`).
pub fn read_claude_statusline(
    path: &Path,
    account: &str,
) -> Result<Vec<QuotaSnapshot>, ProbeError> {
    #[derive(Deserialize, Default)]
    struct Limits {
        five_hour: Option<Window>,
        seven_day: Option<Window>,
    }
    #[derive(Deserialize)]
    struct Window {
        used_percentage: f64,
        resets_at: Option<f64>,
    }
    let text = fs::read_to_string(path).map_err(ProbeError::io(path))?;
    let at_ms = mtime_ms(path)?;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(ProbeError::json(path))?;
    let limits = Limits::deserialize(value.get("rate_limits").unwrap_or(&value))
        .map_err(ProbeError::json(path))?;
    let windows = [
        (rolling_5h(), limits.five_hour),
        (QuotaWindowKind::Weekly, limits.seven_day),
    ];
    Ok(windows
        .into_iter()
        .filter_map(|(kind, w)| w.map(|w| (kind, w)))
        .map(|(window, w)| QuotaSnapshot {
            account: account.to_string(),
            window,
            used_pct: w.used_percentage,
            resets_at_ms: w.resets_at.map(secs_to_ms),
            source: ProbeSource::ClaudeStatusline,
            at_ms,
        })
        .collect())
}

/// Owner-written JSON: `[{"kind": "weekly", "used_pct": 40, "resets_at_ms": 1790000000000}]`.
pub fn read_manual(path: &Path, account: &str) -> Result<Vec<QuotaSnapshot>, ProbeError> {
    #[derive(Deserialize)]
    struct Row {
        #[serde(flatten)]
        window: QuotaWindowKind,
        used_pct: f64,
        resets_at_ms: Option<UnixMs>,
    }
    let text = fs::read_to_string(path).map_err(ProbeError::io(path))?;
    let at_ms = mtime_ms(path)?;
    let rows: Vec<Row> = serde_json::from_str(&text).map_err(ProbeError::json(path))?;
    Ok(rows
        .into_iter()
        .map(|r| QuotaSnapshot {
            account: account.to_string(),
            window: r.window,
            used_pct: r.used_pct,
            resets_at_ms: r.resets_at_ms,
            source: ProbeSource::Manual,
            at_ms,
        })
        .collect())
}

/// Newest `token_count` `rate_limits` event among the newest Codex session files.
pub fn read_codex_sessions(dir: &Path, account: &str) -> Result<Vec<QuotaSnapshot>, ProbeError> {
    let mut files = jsonl_files(dir)?;
    files.sort_unstable_by_key(|(mtime, _)| std::cmp::Reverse(*mtime));
    let mut best: Vec<QuotaSnapshot> = Vec::new();
    for (mtime, path) in files.into_iter().take(CODEX_CANDIDATES) {
        match last_codex_event(&path, account, mtime) {
            Ok(Some(snaps)) if snaps.first().map(|s| s.at_ms) > best.first().map(|b| b.at_ms) => {
                best = snaps
            }
            Ok(_) => {}
            Err(e) => tracing::debug!("codex quota probe: {e}"),
        }
    }
    Ok(best)
}

/// Reads growing tails until a rate_limits event shows up or the cap is reached.
fn last_codex_event(
    path: &Path,
    account: &str,
    mtime: UnixMs,
) -> Result<Option<Vec<QuotaSnapshot>>, ProbeError> {
    let mut file = File::open(path).map_err(ProbeError::io(path))?;
    let len = file.metadata().map_err(ProbeError::io(path))?.len();
    let mut tail = TAIL_START_BYTES;
    loop {
        let text = read_tail(&mut file, len, tail).map_err(ProbeError::io(path))?;
        if let Some(snaps) = text
            .lines()
            .rev()
            .find_map(|l| parse_codex_line(l, account, mtime))
        {
            return Ok(Some(snaps));
        }
        if tail >= len || tail >= TAIL_MAX_BYTES {
            return Ok(None);
        }
        tail = (tail * 4).min(TAIL_MAX_BYTES);
    }
}

fn read_tail(file: &mut File, len: u64, bytes: u64) -> std::io::Result<String> {
    file.seek(SeekFrom::Start(len.saturating_sub(bytes)))?;
    let mut buf = Vec::new();
    file.by_ref().take(bytes).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// One session line; only `token_count` events with the `codex` limit count.
fn parse_codex_line(line: &str, account: &str, mtime: UnixMs) -> Option<Vec<QuotaSnapshot>> {
    #[derive(Deserialize)]
    struct Line {
        timestamp: Option<String>,
        payload: Option<Payload>,
    }
    #[derive(Deserialize)]
    struct Payload {
        #[serde(rename = "type")]
        kind: Option<String>,
        rate_limits: Option<Limits>,
    }
    #[derive(Deserialize)]
    struct Limits {
        limit_id: Option<String>,
        primary: Option<Window>,
        secondary: Option<Window>,
    }
    #[derive(Deserialize)]
    struct Window {
        used_percent: f64,
        window_minutes: Option<u32>,
        resets_at: Option<f64>,
    }
    if !line.contains("\"rate_limits\"") {
        return None;
    }
    let parsed: Line = serde_json::from_str(line).ok()?;
    let payload = parsed
        .payload
        .filter(|p| p.kind.as_deref() == Some("token_count"))?;
    let limits = payload
        .rate_limits
        .filter(|l| l.limit_id.as_deref().is_none_or(|id| id == "codex"))?;
    let at_ms = parsed
        .timestamp
        .as_deref()
        .and_then(rfc3339_ms)
        .unwrap_or(mtime);
    let snaps: Vec<QuotaSnapshot> = [limits.primary, limits.secondary]
        .into_iter()
        .flatten()
        .filter_map(|w| {
            Some(QuotaSnapshot {
                account: account.to_string(),
                window: window_from_minutes(w.window_minutes?),
                used_pct: w.used_percent,
                resets_at_ms: w.resets_at.map(secs_to_ms),
                source: ProbeSource::CodexSessions,
                at_ms,
            })
        })
        .collect();
    (!snaps.is_empty()).then_some(snaps)
}

/// (mtime, path) of every `*.jsonl` under `root`, symlinks not followed.
fn jsonl_files(root: &Path) -> Result<Vec<(UnixMs, PathBuf)>, ProbeError> {
    fs::metadata(root).map_err(ProbeError::io(root))?;
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() && depth < WALK_MAX_DEPTH {
                stack.push((path, depth + 1));
            } else if kind.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
                if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
                    out.push((system_ms(modified), path));
                }
            }
        }
    }
    Ok(out)
}

fn mtime_ms(path: &Path) -> Result<UnixMs, ProbeError> {
    let modified = fs::metadata(path)
        .and_then(|m| m.modified())
        .map_err(ProbeError::io(path))?;
    Ok(system_ms(modified))
}

fn system_ms(t: SystemTime) -> UnixMs {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn secs_to_ms(secs: f64) -> UnixMs {
    (secs * 1000.0).round() as UnixMs
}

fn rfc3339_ms(s: &str) -> Option<UnixMs> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.timestamp_millis())
}
