//! Domain: quoting for shell words, XML text and systemd values. Pure.

/// True for text that can sit inside a unit file or plist without breaking its structure.
pub fn is_plain_text(s: &str) -> bool {
    !s.chars().any(char::is_control)
}

fn is_shell_safe(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c)
}

/// POSIX shell word: bare when every character is safe, else single-quoted.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(is_shell_safe) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Reads one word written by `shell_quote` (bare, single-quoted, or a mix with `\'`)
/// after any leading spaces. Returns the decoded word and the rest of the input.
pub fn read_shell_word(s: &str) -> Option<(String, &str)> {
    let s = s.trim_start_matches(' ');
    let mut word = String::new();
    let mut chars = s.char_indices();
    let mut end = s.len();
    while let Some((i, c)) = chars.next() {
        match c {
            ' ' => {
                end = i;
                break;
            }
            '\'' => loop {
                let (_, quoted) = chars.next()?;
                if quoted == '\'' {
                    break;
                }
                word.push(quoted);
            },
            '\\' => word.push(chars.next()?.1),
            _ => word.push(c),
        }
    }
    (!word.is_empty()).then(|| (word, &s[end..]))
}

/// Escapes text for an XML element body.
pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn is_systemd_safe(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_@+=:,./-".contains(c)
}

/// One word of an `ExecStart=` line: bare when safe, else double-quoted with `\`, `"`,
/// `%` and `$` escaped so systemd expands nothing.
pub fn systemd_exec_word(s: &str) -> String {
    if !s.is_empty() && s.chars().all(is_systemd_safe) {
        return s.to_string();
    }
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('$', "$$");
    format!("\"{escaped}\"")
}

/// An `Environment=` line for one variable; `%` is doubled so systemd expands no specifier.
pub fn systemd_env_line(name: &str, value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("Environment=\"{name}={escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_words_round_trip() {
        for s in ["/usr/bin/x", "a b", "it's", "$HOME \"q\" `c`", "x;y|z"] {
            let q = shell_quote(s);
            let input = format!("{q} tail");
            let (w, rest) = read_shell_word(&input).unwrap();
            assert_eq!((w.as_str(), rest), (s, " tail"), "{q}");
        }
    }

    #[test]
    fn systemd_quoting_blocks_expansion() {
        assert_eq!(systemd_exec_word("/usr/bin/idlemax"), "/usr/bin/idlemax");
        assert_eq!(systemd_exec_word("/a b/$x%h"), "\"/a b/$$x%%h\"");
        assert_eq!(
            systemd_env_line("PATH", "/a:/b \"c\" %h"),
            "Environment=\"PATH=/a:/b \\\"c\\\" %%h\""
        );
    }
}
