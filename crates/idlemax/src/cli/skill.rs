//! Delivery: `idlemax skill`. Prints the agent skill generated from the same static text the
//! home view uses, or with `--check` compares the committed copy to it so CI fails when the
//! two drift. The skill carries no live state: only the hook and the home view can show that.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use idlemax_core::install::diff::line_diff;
use serde::Serialize;

use super::files::read_optional;
use super::home::{COMMANDS, DESCRIPTION};
use super::{ExitWith, Format, Out};

/// Where the skill is committed, relative to the repository root.
pub const DEFAULT_PATH: &str = "skills/idlemax/SKILL.md";

/// Written as a trigger: terse, and about the user's intent rather than the tool's features.
const TRIGGER: &str = "Use when the user asks what spare capacity their devices have, which background jobs are queued, running or failed, why a job is not running, or how much AI subscription quota can be spent before it resets. Runs idlemax from the shell.";

const INSTALL: &str =
    "curl -fsSL https://raw.githubusercontent.com/mrx-arafat/idlemax/main/scripts/install.sh | sh";

#[derive(Args, Debug)]
pub struct SkillArgs {
    /// Compare the skill file at PATH (default skills/idlemax/SKILL.md) with the generated one
    /// and exit 1 when they differ; without it, print the generated skill
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = DEFAULT_PATH)]
    check: Option<PathBuf>,
}

#[derive(Serialize)]
struct Verdict {
    path: PathBuf,
    current: bool,
}

/// The skill file: frontmatter, then the home view's description and command list.
pub fn generate() -> String {
    let mut out = format!(
        "---\nname: idlemax\ndescription: {TRIGGER}\n---\n\n# idlemax\n\n{DESCRIPTION}.\n\n"
    );
    out.push_str("`idlemax` with no arguments prints live state: each device with its guard verdict, job counts, open advice, spendable AI accounts and the next commands to try. Start there.\n\n");
    out.push_str("## Commands\n\n");
    for (cmd, what) in COMMANDS {
        out.push_str(&format!("- `{cmd}`: {what}\n"));
    }
    out.push_str("\n## Good to know\n\n");
    out.push_str("- Add `--json` to a command for machine-readable output.\n");
    out.push_str(
        "- A job runs only when the guard allows it; `idlemax why <id>` says what holds it.\n",
    );
    out.push_str("- `idlemax local status` still works when the coordinator is down.\n");
    out.push_str("- To see live state at the start of every session, ask the user before running `idlemax hooks install`.\n");
    out.push_str(&format!(
        "\n## Not installed?\n\nInstall the binary with `{INSTALL}`, then run `idlemax init`.\n"
    ));
    out
}

pub fn run(args: SkillArgs, out: &Out) -> Result<()> {
    let generated = generate();
    let Some(path) = args.check else {
        return out.raw(&generated);
    };
    let committed = read_optional(&path)?;
    let current = committed.as_deref() == Some(generated.as_str());
    let verdict = Verdict {
        path: path.clone(),
        current,
    };
    let text = check_text(&path, committed.as_deref(), &generated);
    if out.format() == Format::Json {
        out.emit(&verdict, |_| text.clone(), &[])?;
    } else {
        out.raw(&text)?;
    }
    if !current {
        return Err(ExitWith(1).into());
    }
    Ok(())
}

/// The verdict as lines: `skill: ... is current`, or `error:` with the diff and the fix.
fn check_text(path: &std::path::Path, committed: Option<&str>, generated: &str) -> String {
    let shown = path.display();
    match committed {
        Some(old) if old == generated => format!("skill: {shown} is current"),
        None => format!(
            "error: {shown} does not exist\nhelp: Run `idlemax skill > {shown}` to create it"
        ),
        Some(old) => format!(
            "error: {shown} is out of date\n{}\nhelp: Run `idlemax skill > {shown}` to regenerate it",
            line_diff(old, generated, 2).trim_end()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_skill_is_trigger_shaped_static_and_lists_every_command() {
        let text = generate();
        assert!(
            text.starts_with("---\nname: idlemax\ndescription: Use when the user asks "),
            "{text}"
        );
        assert_eq!(
            text.matches("\n---\n").count(),
            1,
            "frontmatter closes once"
        );
        for (cmd, what) in COMMANDS {
            assert!(text.contains(&format!("- `{cmd}`: {what}\n")), "{cmd}");
        }
        // No live state and no em dash: those belong to the hook and the repo style.
        assert!(
            !text.contains("devices[")
                && !text.contains("coordinator:")
                && !text.contains('\u{2014}')
        );
        assert_eq!(text, generate(), "deterministic");
    }
}
