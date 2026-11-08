//! Delivery: TOON primitives, keys and their quoting and number rules (spec sections 2, 7).

use super::Node;

/// An object key or field name: bare when it is an identifier, else quoted.
pub fn key(k: &str) -> String {
    let mut chars = k.chars();
    let ident = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    if ident {
        k.to_string()
    } else {
        quoted(k)
    }
}

/// A primitive cell or value. Containers never reach here.
pub fn primitive(n: &Node) -> String {
    match n {
        Node::Null | Node::Arr(_) | Node::Obj(_) => "null".into(),
        Node::Bool(b) => b.to_string(),
        Node::Int(i) => i.to_string(),
        Node::Uint(u) => u.to_string(),
        Node::Float(f) => float(*f),
        Node::Str(s) => string(s),
    }
}

/// A string value; the document delimiter is always the comma.
pub fn string(s: &str) -> String {
    if needs_quotes(s) {
        quoted(s)
    } else {
        s.to_string()
    }
}

fn needs_quotes(s: &str) -> bool {
    s.is_empty()
        || s.starts_with([' ', '\t'])
        || s.ends_with([' ', '\t'])
        || s.starts_with(['-', '#'])
        || matches!(s, "true" | "false" | "null")
        || s.chars().any(|c| {
            (c as u32) < 0x20 || matches!(c, ',' | ':' | '"' | '\\' | '[' | ']' | '{' | '}')
        })
        || numeric_like(s)
}

/// `^[+-]?[0-9]+(\.[0-9]+)?(e[+-]?[0-9]+)?$`, case-insensitive, ASCII digits.
fn numeric_like(s: &str) -> bool {
    let digits = |s: &str| (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then_some(());
    let s = s.strip_prefix(['+', '-']).unwrap_or(s);
    let (mantissa, exp) = match s.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e.strip_prefix(['+', '-']).unwrap_or(e))),
        None => (s, None),
    };
    let (int, frac) = match mantissa.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (mantissa, None),
    };
    digits(int).is_some()
        && frac.is_none_or(|f| digits(f).is_some())
        && exp.is_none_or(|e| digits(e).is_some())
}

fn quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Canonical decimal for 1e-6 <= |n| < 1e21; exponent form outside; null for NaN and infinity.
fn float(f: f64) -> String {
    if !f.is_finite() {
        return "null".into();
    }
    if f == 0.0 {
        return "0".into();
    }
    if (1e-6..1e21).contains(&f.abs()) {
        return f.to_string();
    }
    let s = format!("{f:e}");
    match s.split_once('e') {
        Some((mantissa, exp)) if !exp.starts_with('-') => format!("{mantissa}e+{exp}"),
        _ => s,
    }
}
