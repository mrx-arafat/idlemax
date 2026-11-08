//! Delivery: clap usage errors for machine formats. The error and the failing command's valid
//! flags go to stdout in one reply, so a caller fixes its call without a second `--help` run.

use clap::{Arg, Command};

use super::failure::CliError;
use super::Format;

/// The format to use for a command line that did not parse.
pub fn sniff_format(args: &[String], stdout_is_terminal: bool) -> Format {
    let mut explicit = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json = true,
            "--format" => explicit = it.next().and_then(|v| parse_format(v)),
            other => {
                if let Some(v) = other.strip_prefix("--format=") {
                    explicit = parse_format(v);
                }
            }
        }
    }
    Format::resolve(explicit, json, stdout_is_terminal)
}

fn parse_format(v: &str) -> Option<Format> {
    match v {
        "table" => Some(Format::Table),
        "toon" => Some(Format::Toon),
        "json" => Some(Format::Json),
        _ => None,
    }
}

/// The usage error for `err`, with the valid flags of the command the line was aimed at.
pub fn usage_error(err: &clap::Error, root: &mut Command, args: &[String]) -> CliError {
    let text = err.to_string();
    let what = text
        .lines()
        .take_while(|l| !l.trim().is_empty())
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ")
        .trim_start_matches("error: ")
        .to_string();
    let tip = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("tip: "))
        .map(str::to_string);
    root.build();
    let (cmd, path) = target(root, args);
    let mut help = valid_of(cmd, &path);
    if let Some(tip) = tip {
        help = format!("{tip}; {help}");
    }
    CliError::usage(what, help)
}

/// The deepest subcommand the arguments name, and its words ("idlemax", "job", "add").
fn target<'a>(root: &'a Command, args: &[String]) -> (&'a Command, Vec<String>) {
    let mut cmd = root;
    let mut path = vec![root.get_name().to_string()];
    for a in args {
        if let Some(sub) = cmd.find_subcommand(a) {
            cmd = sub;
            path.push(sub.get_name().to_string());
        }
    }
    (cmd, path)
}

fn valid_of(cmd: &Command, path: &[String]) -> String {
    let flags: Vec<String> = cmd
        .get_arguments()
        .filter(|a| !a.is_hide_set() && !matches!(a.get_id().as_str(), "help" | "version"))
        .map(flag)
        .collect();
    let subs: Vec<&str> = cmd
        .get_subcommands()
        .filter(|s| !s.is_hide_set() && s.get_name() != "help")
        .map(Command::get_name)
        .collect();
    let mut parts = Vec::new();
    if !subs.is_empty() {
        parts.push(format!("commands: {}", subs.join(", ")));
    }
    if !flags.is_empty() {
        parts.push(format!("args: {}", flags.join(", ")));
    }
    format!(
        "valid for `{}` - {} (--help always allowed)",
        path.join(" "),
        parts.join("; ")
    )
}

fn flag(a: &Arg) -> String {
    let name = match (a.get_long(), a.is_positional()) {
        (Some(long), _) => format!("--{long}"),
        (None, true) => format!("<{}>", a.get_id().as_str().to_uppercase()),
        (None, false) => a.get_short().map_or_else(String::new, |s| format!("-{s}")),
    };
    let takes_value = a.get_num_args().is_some_and(|n| n.takes_values()) && !a.is_positional();
    if takes_value {
        let value = a
            .get_value_names()
            .and_then(|v| v.first())
            .map_or_else(|| a.get_id().as_str().to_uppercase(), |v| v.to_string());
        format!("{name} <{value}>")
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{Parser, Subcommand};

    #[derive(Parser)]
    #[command(name = "tool")]
    struct Cli {
        #[arg(long, global = true)]
        json: bool,
        #[command(subcommand)]
        cmd: Cmd,
    }

    #[derive(Subcommand)]
    enum Cmd {
        /// List things
        List {
            #[arg(long)]
            status: Option<String>,
            #[arg(long, default_value_t = 5)]
            limit: u32,
        },
        Stop {
            id: i64,
        },
    }

    fn explain(args: &[&str]) -> CliError {
        use clap::CommandFactory;
        let err = Cli::try_parse_from(args).err().expect("must fail to parse");
        let rest: Vec<String> = args.iter().skip(1).map(|a| a.to_string()).collect();
        usage_error(&err, &mut Cli::command(), &rest)
    }

    #[test]
    fn an_unknown_flag_lists_the_valid_flags_of_that_command() {
        let e = explain(&["tool", "list", "--stat", "x"]);
        assert!(e.what.contains("--stat"), "{}", e.what);
        let help = e.help.unwrap();
        for want in [
            "`tool list`",
            "--status <STATUS>",
            "--limit <LIMIT>",
            "--json",
            "--help always allowed",
        ] {
            assert!(help.contains(want), "{help}");
        }
        assert!(help.contains("--status"), "{help}");
        assert_eq!(e.code, super::super::failure::USAGE);
    }

    #[test]
    fn a_missing_argument_names_the_command_and_an_unknown_command_lists_commands() {
        let e = explain(&["tool", "stop"]);
        assert!(e.help.unwrap().contains("`tool stop` - args: <ID>, --json"));
        let e = explain(&["tool", "lst"]);
        let help = e.help.unwrap();
        assert!(help.contains("commands: list, stop"), "{help}");
    }

    #[test]
    fn the_format_comes_from_the_raw_arguments() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            sniff_format(&args(&["jobs", "--format", "json"]), true),
            Format::Json
        );
        assert_eq!(
            sniff_format(&args(&["--format=table", "jobs"]), false),
            Format::Table
        );
        assert_eq!(sniff_format(&args(&["jobs", "--json"]), true), Format::Json);
        assert_eq!(sniff_format(&args(&["jobs"]), false), Format::Toon);
        assert_eq!(sniff_format(&args(&["jobs"]), true), Format::Table);
    }
}
