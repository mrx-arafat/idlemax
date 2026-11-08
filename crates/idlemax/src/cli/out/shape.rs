//! Delivery: size limits for TOON output (AXI "content truncation").

use super::toon::Node;

/// Longest string printed in TOON before it is cut; `--full` lifts it.
pub const TEXT_LIMIT: usize = 500;

/// Cuts every string longer than `limit` characters, keeping its start and saying how long
/// it was. Returns the tree and how many strings were cut.
pub fn truncate(node: Node, limit: usize) -> (Node, usize) {
    match node {
        Node::Str(s) => match s.char_indices().nth(limit) {
            Some((cut, _)) => {
                let total = s.chars().count();
                (
                    Node::Str(format!("{}... (truncated, {total} chars total)", &s[..cut])),
                    1,
                )
            }
            None => (Node::Str(s), 0),
        },
        Node::Arr(items) => {
            let (items, cut) = truncate_all(items, limit);
            (Node::Arr(items), cut)
        }
        Node::Obj(fields) => {
            let (keys, values): (Vec<_>, Vec<_>) = fields.into_iter().unzip();
            let (values, cut) = truncate_all(values, limit);
            (Node::Obj(keys.into_iter().zip(values).collect()), cut)
        }
        other => (other, 0),
    }
}

fn truncate_all(nodes: Vec<Node>, limit: usize) -> (Vec<Node>, usize) {
    let mut cut = 0;
    let nodes = nodes
        .into_iter()
        .map(|n| {
            let (n, c) = truncate(n, limit);
            cut += c;
            n
        })
        .collect();
    (nodes, cut)
}

/// The invocation with `--full` added, for the "see everything" hint.
pub fn with_full(args: &[String]) -> String {
    let quote = |a: &String| {
        if a.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=,@%+".contains(c))
        {
            a.clone()
        } else {
            format!("'{}'", a.replace('\'', "'\\''"))
        }
    };
    let mut words: Vec<String> = args.iter().filter(|a| *a != "--full").map(quote).collect();
    words.insert(0, "idlemax".into());
    words.push("--full".into());
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Node {
        Node::Str(s.to_string())
    }

    #[test]
    fn long_text_is_cut_with_its_total_and_short_text_is_left_alone() {
        let tree = Node::Obj(vec![
            ("a".into(), text(&"é".repeat(12))),
            ("b".into(), text("short")),
        ]);
        let (cut, n) = truncate(tree, 10);
        assert_eq!(n, 1);
        let want = format!("{}... (truncated, 12 chars total)", "é".repeat(10));
        assert_eq!(
            cut,
            Node::Obj(vec![("a".into(), text(&want)), ("b".into(), text("short"))])
        );
        assert_eq!(truncate(text("1234567890"), 10).1, 0);
    }

    #[test]
    fn the_full_hint_repeats_the_command_with_quoting() {
        let args: Vec<String> = ["why", "7", "--reason", "a b"].map(String::from).to_vec();
        assert_eq!(with_full(&args), "idlemax why 7 --reason 'a b' --full");
        let again: Vec<String> = ["jobs", "--full"].map(String::from).to_vec();
        assert_eq!(with_full(&again), "idlemax jobs --full");
    }
}
