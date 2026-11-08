//! Domain: a small line diff for `--dry-run` output. Pure.

/// The changed lines between `old` and `new` with `context` unchanged lines around them,
/// or an empty string when they are equal. It shows one hunk: everything between the
/// common head and the common tail.
pub fn line_diff(old: &str, new: &str, context: usize) -> String {
    if old == new {
        return String::new();
    }
    let a: Vec<&str> = old.split('\n').collect();
    let b: Vec<&str> = new.split('\n').collect();
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (a_end, b_end) = (a.len() - tail, b.len() - tail);
    let from = head.saturating_sub(context);
    let mut out = format!("@@ line {} @@\n", from + 1);
    for line in &a[from..head] {
        out.push_str(&format!(" {line}\n"));
    }
    for line in &a[head..a_end] {
        out.push_str(&format!("-{line}\n"));
    }
    for line in &b[head..b_end] {
        out.push_str(&format!("+{line}\n"));
    }
    for line in a[a_end..].iter().take(context) {
        out.push_str(&format!(" {line}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_only_the_changed_lines_with_context() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nb\nX\nd\ne\n";
        assert_eq!(line_diff(old, new, 1), "@@ line 2 @@\n b\n-c\n+X\n d\n");
        assert_eq!(line_diff(old, old, 1), "");
    }
}
