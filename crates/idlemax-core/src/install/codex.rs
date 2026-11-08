//! Domain: the `[features] hooks = true` switch in Codex's `config.toml`, which Codex needs
//! before it runs `hooks.json`. Pure text in, text out. The edit is line based, so comments and
//! layout survive, and a real TOML parse checks both the input and the result.

use super::hooks::HooksError;

const NEWLINE: &str = "\n";

fn parse(text: &str) -> Result<toml::Table, HooksError> {
    toml::from_str(text).map_err(|e| HooksError::Invalid {
        format: "TOML",
        why: e.message().to_string(),
    })
}

/// True when `[features] hooks` is `true`; false for a missing file or any other value.
pub fn hooks_enabled(text: Option<&str>) -> bool {
    text.and_then(|t| parse(t).ok())
        .and_then(|t| t.get("features")?.get("hooks")?.as_bool())
        == Some(true)
}

/// The `config.toml` text with `[features] hooks = true`; the same text when it already is.
pub fn plan_enable(text: Option<&str>) -> Result<String, HooksError> {
    let text = text.unwrap_or("");
    let table = parse(text)?;
    if table.get("features").is_some_and(|f| !f.is_table()) {
        return Err(HooksError::UnexpectedShape(
            "\"features\" is not a table".into(),
        ));
    }
    if hooks_enabled(Some(text)) {
        return Ok(text.to_string());
    }
    let nl = if text.contains("\r\n") {
        "\r\n"
    } else {
        NEWLINE
    };
    let out = if text.trim().is_empty() {
        format!("[features]{nl}hooks = true{nl}")
    } else {
        edit(text, nl)
    };
    if hooks_enabled(Some(&out)) {
        Ok(out)
    } else {
        Err(HooksError::Verify(
            "hooks is not enabled in the result".into(),
        ))
    }
}

fn edit(text: &str, nl: &str) -> String {
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect();
    let (mut in_features, mut saw_features) = (false, false);
    for i in 0..lines.len() {
        if let Some(name) = table_name(&lines[i]) {
            if in_features {
                lines.insert(i, "hooks = true".into());
                return lines.join(nl);
            }
            in_features = name == "features";
            saw_features |= in_features;
        } else if in_features && is_hooks_false(&lines[i]) {
            lines[i] = lines[i].replacen("false", "true", 1);
            return lines.join(nl);
        }
    }
    if saw_features {
        let end = if lines.last().is_some_and(String::is_empty) {
            lines.len() - 1
        } else {
            lines.len()
        };
        lines.insert(end, "hooks = true".into());
        return lines.join(nl);
    }
    let gap = if text.ends_with('\n') {
        nl.to_string()
    } else {
        format!("{nl}{nl}")
    };
    format!("{text}{gap}[features]{nl}hooks = true{nl}")
}

/// `features` for `[features]` or `[[features]]`; None for anything that is not a table header
/// (a row of a multi-line array looks like one but has text after the closing bracket).
fn table_name(line: &str) -> Option<&str> {
    let body = line.trim_start().strip_prefix('[')?;
    let body = body.strip_prefix('[').unwrap_or(body);
    let (name, rest) = body.split_once(']')?;
    let rest = rest.trim_start_matches(']').trim();
    (rest.is_empty() || rest.starts_with('#')).then(|| name.trim())
}

fn is_hooks_false(line: &str) -> bool {
    let rest = line
        .trim()
        .strip_prefix("hooks")
        .map(str::trim_start)
        .and_then(|r| r.strip_prefix('='))
        .map(str::trim_start);
    rest.and_then(|r| r.strip_prefix("false"))
        .is_some_and(|r| r.trim().is_empty() || r.trim().starts_with('#'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enables_hooks_in_every_layout_and_keeps_the_rest() {
        let cases = [
            (None, "[features]\nhooks = true\n"),
            (Some(""), "[features]\nhooks = true\n"),
            (
                Some("model = \"o3\"\n"),
                "model = \"o3\"\n\n[features]\nhooks = true\n",
            ),
            (
                Some("model = \"o3\""),
                "model = \"o3\"\n\n[features]\nhooks = true\n",
            ),
            (
                Some("[features]\nweb = true\n\n[tui]\nx = 1\n"),
                "[features]\nweb = true\n\nhooks = true\n[tui]\nx = 1\n",
            ),
            (
                Some("[features]\nweb = true\n"),
                "[features]\nweb = true\nhooks = true\n",
            ),
            (
                Some("[features]\nhooks = false # off\n"),
                "[features]\nhooks = true # off\n",
            ),
            (
                Some("[features]\r\nweb = 1\r\n"),
                "[features]\r\nweb = 1\r\nhooks = true\r\n",
            ),
            (
                Some("# top\n[a]\nlist = [\n  [1, 2],\n]\n"),
                "# top\n[a]\nlist = [\n  [1, 2],\n]\n\n[features]\nhooks = true\n",
            ),
        ];
        for (before, after) in cases {
            assert_eq!(plan_enable(before).unwrap(), after, "{before:?}");
            assert!(hooks_enabled(Some(after)));
            assert_eq!(
                plan_enable(Some(after)).unwrap(),
                after,
                "idempotent {before:?}"
            );
        }
    }

    #[test]
    fn broken_or_odd_files_are_refused() {
        assert!(matches!(
            plan_enable(Some("[features")),
            Err(HooksError::Invalid { format: "TOML", .. })
        ));
        assert!(matches!(
            plan_enable(Some("features = 3\n")),
            Err(HooksError::UnexpectedShape(_))
        ));
        assert!(!hooks_enabled(None) && !hooks_enabled(Some("[features]\nhooks = false\n")));
    }
}
