//! Domain: reads back what `render` wrote into a service unit - the binary the service runs
//! and the PATH it starts with. Pure; hand-written units in the same syntax work too.

use super::service::Platform;

/// What an installed unit file says about how its service starts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnitFacts {
    /// The program the service runs (`ExecStart` or the first `ProgramArguments` entry).
    pub bin: Option<String>,
    /// The `PATH` in its environment, when the unit sets one.
    pub path: Option<String>,
}

pub fn read_unit(platform: Platform, text: &str) -> UnitFacts {
    match platform {
        Platform::Launchd => UnitFacts {
            bin: plist_string(text, "ProgramArguments"),
            path: plist_string(text, "PATH"),
        },
        Platform::SystemdUser => UnitFacts {
            bin: text
                .lines()
                .find_map(|l| l.strip_prefix("ExecStart="))
                .and_then(exec_word),
            path: text.lines().find_map(|l| env_value(l, "PATH")),
        },
    }
}

/// The first `<string>` after `<key>name</key>`, unescaped.
fn plist_string(text: &str, key: &str) -> Option<String> {
    let after = &text[text.find(&format!("<key>{key}</key>"))?..];
    let start = after.find("<string>")? + "<string>".len();
    let len = after[start..].find("</string>")?;
    Some(
        after[start..start + len]
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    )
}

/// The program of an `ExecStart=` value: after systemd's `-@+!:` prefixes, one bare or
/// double-quoted word.
fn exec_word(value: &str) -> Option<String> {
    let value = value
        .trim_start()
        .trim_start_matches(['-', '@', '+', '!', ':']);
    let word = match value.strip_prefix('"') {
        Some(quoted) => unescape(quoted)?.0,
        None => value.split_whitespace().next()?.replace("$$", "$"),
    };
    (!word.is_empty()).then_some(word)
}

/// The value of `name` in an `Environment=` line, bare (`NAME=v`) or quoted (`"NAME=v"`).
fn env_value(line: &str, name: &str) -> Option<String> {
    let rest = line.strip_prefix("Environment=")?.trim_start();
    let prefix = format!("{name}=");
    if let Some(quoted) = rest.strip_prefix('"') {
        let (assignment, _) = unescape(quoted)?;
        return assignment.strip_prefix(&prefix).map(str::to_string);
    }
    rest.split_whitespace()
        .find_map(|word| word.strip_prefix(&prefix))
        .map(str::to_string)
}

/// Text up to the closing quote with `\x` -> `x` and `%%` -> `%`, and what follows it.
fn unescape(quoted: &str) -> Option<(String, &str)> {
    let mut out = String::new();
    let mut chars = quoted.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &quoted[i + 1..])),
            '\\' => out.push(chars.next()?.1),
            '%' if chars.peek().is_some_and(|(_, n)| *n == '%') => {
                chars.next();
                out.push('%');
            }
            '$' if chars.peek().is_some_and(|(_, n)| *n == '$') => {
                chars.next();
                out.push('$');
            }
            _ => out.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::service::{render, ServiceEnv, ServiceKind};

    fn env() -> ServiceEnv {
        ServiceEnv {
            agent_jobs: 1,
            agent_ram_mib: 512,
            agent_tasks_max: 256,
            bin: "/opt/my tools/idlemax".into(),
            home: "/home/u".into(),
            state_dir: "/data & more/idlemax".into(),
            path: Some("/usr/bin:/opt/n&b/bin".into()),
            vars: vec![],
            uid: 501,
        }
    }

    #[test]
    fn reads_back_the_binary_and_path_that_render_wrote_on_both_platforms() {
        for platform in [Platform::Launchd, Platform::SystemdUser] {
            let unit = render(platform, ServiceKind::Agent, &env()).unwrap();
            assert_eq!(
                read_unit(platform, &unit.content),
                UnitFacts {
                    bin: Some("/opt/my tools/idlemax".into()),
                    path: Some("/usr/bin:/opt/n&b/bin".into()),
                },
                "{platform:?}"
            );
        }
    }

    #[test]
    fn a_hand_written_system_unit_has_a_binary_and_no_path() {
        let text = "[Service]\nExecStart=/usr/local/bin/idlemax agent\nEnvironment=IDLEMAX_HOME=/var/lib/idlemax\n";
        let facts = read_unit(Platform::SystemdUser, text);
        assert_eq!(facts.bin.as_deref(), Some("/usr/local/bin/idlemax"));
        assert_eq!(facts.path, None);
        assert_eq!(read_unit(Platform::SystemdUser, ""), UnitFacts::default());
    }
}
