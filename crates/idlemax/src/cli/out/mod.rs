//! Delivery: the output boundary every command prints through.
//!
//! A command hands [`Out::emit`] the typed API value, a table renderer and its next-step
//! hints. The active [`Format`] decides what is printed:
//! - `table`: the human table (default when stdout is a terminal), hints omitted;
//! - `toon`: TOON for agents (default when stdout is piped), long text cut unless `--full`,
//!   hints as a trailing `help[N]:` list;
//! - `json`: the API value as pretty JSON, untouched.
//!
//! Keep decisions out of here: callers pass finished values and finished hint strings.

mod failure;
pub mod hints;
pub mod lists;
mod shape;
pub mod toon;
mod usage;
pub mod views;

use std::io::{ErrorKind, Write};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::ValueEnum;
use serde::Serialize;

pub use failure::{unreachable, CliError, Peer};
use toon::Node;
pub use usage::{sniff_format, usage_error};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Table,
    Toon,
    Json,
}

impl Format {
    /// `--format` wins, then `--json`, then the terminal check: tables for people, TOON for
    /// whatever reads a pipe.
    pub fn resolve(explicit: Option<Format>, json_flag: bool, stdout_is_terminal: bool) -> Format {
        match (explicit, json_flag, stdout_is_terminal) {
            (Some(f), _, _) => f,
            (None, true, _) => Format::Json,
            (None, false, true) => Format::Table,
            (None, false, false) => Format::Toon,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Out {
    format: Format,
    full: bool,
    fields: Vec<String>,
}

impl Out {
    pub fn new(format: Format, full: bool, fields: Vec<String>) -> Self {
        Self {
            format,
            full,
            fields,
        }
    }

    pub fn is_toon(&self) -> bool {
        self.format == Format::Toon
    }

    /// The extra list columns asked for with `--fields`.
    pub fn fields(&self) -> &[String] {
        &self.fields
    }

    pub fn format(&self) -> Format {
        self.format
    }

    /// Prints `value` in the active format. `hints` are complete next-step sentences such
    /// as ``Run `idlemax why <id>` to see why it waits``; only TOON shows them.
    pub fn emit<T: Serialize>(
        &self,
        value: &T,
        table: impl FnOnce(&T) -> String,
        hints: &[String],
    ) -> Result<()> {
        self.emit_with(value, table, || Ok(Node::of(value)?), hints)
    }

    /// Like [`Out::emit`], with a curated TOON `view` in place of the raw value.
    pub fn emit_with<T: Serialize>(
        &self,
        value: &T,
        table: impl FnOnce(&T) -> String,
        view: impl FnOnce() -> Result<Node>,
        hints: &[String],
    ) -> Result<()> {
        print_line(&self.render(value, table, view, hints)?)
    }

    /// The text `emit_with` prints. Table and TOON text is for a terminal or an agent, so it
    /// carries no control characters; JSON keeps every value and serde escapes them.
    fn render<T: Serialize>(
        &self,
        value: &T,
        table: impl FnOnce(&T) -> String,
        view: impl FnOnce() -> Result<Node>,
        hints: &[String],
    ) -> Result<String> {
        // `--fields` picks TOON columns, but a bad one is a usage error in every format.
        if self.format != Format::Toon && !self.fields.is_empty() {
            view()?;
            return self.render_plain(value, table);
        }
        Ok(match self.format {
            Format::Toon => printable(&self.toon(view()?, hints)),
            _ => self.render_plain(value, table)?,
        })
    }

    fn render_plain<T: Serialize>(
        &self,
        value: &T,
        table: impl FnOnce(&T) -> String,
    ) -> Result<String> {
        Ok(match self.format {
            Format::Json => serde_json::to_string_pretty(value).context("encoding JSON")?,
            _ => printable(&table(value)),
        })
    }

    /// Prints the error for `e` in the active format and returns the exit code: machine
    /// formats on stdout where the caller reads it, the table format on stderr.
    pub fn fail(&self, e: &anyhow::Error) -> ExitCode {
        self.report(&CliError::from_error(e))
    }

    pub fn report(&self, err: &CliError) -> ExitCode {
        let text = self.error_text(err);
        if self.format == Format::Table {
            eprintln!("{text}");
        } else {
            let _ = print_line(&text);
        }
        ExitCode::from(err.code)
    }

    /// The error as printed in the active format, without control characters outside JSON.
    fn error_text(&self, err: &CliError) -> String {
        let text = err.render(self.format);
        if self.format == Format::Json {
            text
        } else {
            printable(&text)
        }
    }

    /// Prints `view` as TOON whatever the format, for output that is only ever read by an
    /// agent, such as the session hook summary.
    pub fn emit_toon(&self, view: Node, hints: &[String]) -> Result<()> {
        print_line(&printable(&self.toon(view, hints)))
    }

    /// Prints content the caller asked for verbatim (a log, a note), whatever the format.
    pub fn raw(&self, text: &str) -> Result<()> {
        print_line(text.strip_suffix('\n').unwrap_or(text))
    }

    fn toon(&self, view: Node, hints: &[String]) -> String {
        let (view, cut) = if self.full {
            (view, 0)
        } else {
            shape::truncate(view, shape::TEXT_LIMIT)
        };
        let mut hints = hints.to_vec();
        if cut > 0 {
            let args: Vec<String> = std::env::args().skip(1).collect();
            hints.push(format!(
                "Run `{}` to see the complete text of {cut} cut field(s)",
                shape::with_full(&args)
            ));
        }
        let mut text = toon::encode(&view);
        if !hints.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&toon::list("help", &hints));
        }
        text
    }
}

/// `text` without control characters other than newline and tab. Names, ids, messages and
/// advice titles come from other devices and from GitHub; a raw ESC in them would run a
/// terminal escape sequence (set the title, clear the screen, write the clipboard).
pub fn printable(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// One line to stdout; a closed pipe (`| head`) ends output quietly instead of panicking.
pub fn print_line(text: &str) -> Result<()> {
    match writeln!(std::io::stdout().lock(), "{text}") {
        Err(e) if e.kind() == ErrorKind::BrokenPipe => Ok(()),
        other => other.context("writing to stdout"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_prefers_the_flag_then_json_then_the_terminal() {
        let r = Format::resolve;
        assert_eq!(r(Some(Format::Toon), true, true), Format::Toon);
        assert_eq!(r(None, true, true), Format::Json);
        assert_eq!(r(None, false, true), Format::Table);
        assert_eq!(r(None, false, false), Format::Toon);
    }

    #[test]
    fn printable_drops_terminal_control_sequences_and_keeps_the_text_layout() {
        // ESC starts every CSI and OSC sequence (title, clear screen, clipboard), CR rewrites a
        // line, 0x9b is the one-byte CSI: none may reach a terminal from API-supplied text.
        let hostile = "evil\x1b]0;PWNED\x07\x1b[2Jx\u{9b}31m\r\nline\ttab \u{7f}ünï 日本";
        assert_eq!(
            printable(hostile),
            "evil]0;PWNED[2Jx31m\nline\ttab ünï 日本"
        );
        assert_eq!(printable("plain text 100%"), "plain text 100%");
    }

    /// Names, ids and messages come from other devices and from GitHub. The table and TOON
    /// formats are for a terminal or an agent, so neither may carry a raw escape; JSON keeps
    /// the value and serde escapes the control characters.
    #[test]
    fn every_format_but_raw_keeps_escape_sequences_out_of_the_output() {
        let value = serde_json::json!({ "name": "evil\x1b]0;PWNED\x07\x1b[2Jcleared" });
        let table = |v: &serde_json::Value| format!("name: {}", v["name"].as_str().unwrap());
        for format in [Format::Table, Format::Toon] {
            let out = Out::new(format, false, Vec::new());
            let text = out
                .render(&value, table, || Ok(Node::of(&value)?), &[])
                .unwrap();
            assert!(text.contains("cleared"), "{format:?}: {text}");
            assert!(
                !text.contains('\x1b') && !text.contains('\x07'),
                "{format:?}: {text:?}"
            );
        }
        let json = Out::new(Format::Json, false, Vec::new())
            .render(&value, table, || Ok(Node::of(&value)?), &[])
            .unwrap();
        assert!(!json.contains('\x1b'), "{json:?}");
        assert!(
            json.contains("\\u001b"),
            "JSON keeps the value, escaped: {json}"
        );
        // The same cleaning applies to an error shown in the table format.
        let err = CliError::from_error(&anyhow::anyhow!("bad \x1b[2Jthing"));
        assert!(!Out::new(Format::Table, false, Vec::new())
            .error_text(&err)
            .contains('\x1b'));
    }

    /// README: a bad `--fields` is a usage error (exit 2) in every format, not only in TOON.
    #[test]
    fn a_bad_fields_list_fails_in_every_format() {
        let value = serde_json::json!({ "id": 1 });
        let bad = || {
            Err(anyhow::anyhow!(CliError::usage(
                "unknown field `bogus`",
                "valid --fields: id"
            )))
        };
        for format in [Format::Json, Format::Table, Format::Toon] {
            let out = Out::new(format, false, vec!["bogus".into()]);
            assert!(
                out.render(&value, |_| String::new(), bad, &[]).is_err(),
                "{format:?}"
            );
        }
        let plain = Out::new(Format::Json, false, Vec::new());
        assert!(plain.render(&value, |_| String::new(), bad, &[]).is_ok());
    }
}
