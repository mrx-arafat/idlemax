//! Delivery: TOON v4.4 encoder (https://toonformat.dev), comma delimiter, 2-space indent.
//! Pure: a [`Node`] tree in, text out. Forms follow the value's shape: inline primitive
//! arrays, tables for uniform objects (with nested field groups), keyed tables for objects
//! of uniform objects, and list form for everything else.

#![allow(dead_code)] // wired into the output boundary in the next commit

mod scalar;
#[cfg(test)]
mod tests;
mod value;

pub use value::Node;

use scalar::{key, primitive};

/// (depth, text) lines, joined and indented by [`render`].
type Lines = Vec<(usize, String)>;

/// A table column: a leaf, or a nested field group when `sub` is not empty.
struct Col {
    name: String,
    sub: Vec<Col>,
}

pub fn encode(root: &Node) -> String {
    let mut lines = Lines::new();
    match root {
        Node::Obj(fields) if fields.is_empty() => {}
        Node::Obj(fields) => object(&mut lines, None, fields, 0),
        Node::Arr(items) => array(&mut lines, None, items, 0),
        scalar => lines.push((0, primitive(scalar))),
    }
    render(&lines)
}

fn render(lines: &Lines) -> String {
    let text: Vec<String> = lines
        .iter()
        .map(|(depth, text)| format!("{}{text}", "  ".repeat(*depth)))
        .collect();
    text.join("\n")
}

/// An object's fields at `depth`, as a keyed table when it qualifies.
fn object(lines: &mut Lines, name: Option<&str>, fields: &[(String, Node)], depth: usize) {
    if let Some(cols) = keyed_columns(fields) {
        let head = format!(
            "{}[{}:]{{{}}}:",
            name.unwrap_or(""),
            fields.len(),
            field_list(&cols)
        );
        lines.push((depth, head));
        for (k, v) in fields {
            let cells = cells(v, &cols);
            lines.push((depth + 1, format!("{}: {}", key(k), cells.join(","))));
        }
        return;
    }
    if let Some(name) = name {
        lines.push((depth, format!("{name}:")));
    }
    let depth = if name.is_some() { depth + 1 } else { depth };
    for (k, v) in fields {
        field(lines, k, v, depth);
    }
}

fn field(lines: &mut Lines, name: &str, value: &Node, depth: usize) {
    let k = key(name);
    match value {
        Node::Obj(f) if f.is_empty() => lines.push((depth, format!("{k}:"))),
        Node::Obj(f) => object(lines, Some(&k), f, depth),
        Node::Arr(items) => array(lines, Some(&k), items, depth),
        scalar => lines.push((depth, format!("{k}: {}", primitive(scalar)))),
    }
}

fn array(lines: &mut Lines, name: Option<&str>, items: &[Node], depth: usize) {
    let head = name.unwrap_or("");
    if items.is_empty() {
        let text = if name.is_some() {
            format!("{head}: []")
        } else {
            "[]".into()
        };
        return lines.push((depth, text));
    }
    let n = items.len();
    if items.iter().all(is_primitive) {
        let values: Vec<String> = items.iter().map(primitive).collect();
        return lines.push((depth, format!("{head}[{n}]: {}", values.join(","))));
    }
    if let Some(cols) = columns(items.iter()) {
        lines.push((depth, format!("{head}[{n}]{{{}}}:", field_list(&cols))));
        for item in items {
            lines.push((depth + 1, cells(item, &cols).join(",")));
        }
        return;
    }
    lines.push((depth, format!("{head}[{n}]:")));
    for item in items {
        list_item(lines, item, depth + 1);
    }
}

/// One `- ` item; an object's first field rides on the hyphen line.
fn list_item(lines: &mut Lines, item: &Node, depth: usize) {
    match item {
        Node::Obj(f) if f.is_empty() => lines.push((depth, "-".into())),
        Node::Obj(fields) => {
            let mut inner = Lines::new();
            for (k, v) in fields {
                field(&mut inner, k, v, depth + 1);
            }
            if let Some(first) = inner.first_mut() {
                *first = (depth, format!("- {}", first.1));
            }
            lines.extend(inner);
        }
        Node::Arr(items) if items.is_empty() => lines.push((depth, "- [0]:".into())),
        Node::Arr(items) if items.iter().all(is_primitive) => {
            let values: Vec<String> = items.iter().map(primitive).collect();
            lines.push((depth, format!("- [{}]: {}", items.len(), values.join(","))));
        }
        Node::Arr(items) => {
            lines.push((depth, format!("- [{}]:", items.len())));
            for inner in items {
                list_item(lines, inner, depth + 1);
            }
        }
        scalar => lines.push((depth, format!("- {}", primitive(scalar)))),
    }
}

fn is_primitive(n: &Node) -> bool {
    !matches!(n, Node::Arr(_) | Node::Obj(_))
}

/// Table columns when every item is a non-empty object with the same keys and every column
/// is primitive or itself a uniform object column.
fn columns<'a>(items: impl Iterator<Item = &'a Node> + Clone) -> Option<Vec<Col>> {
    let objects: Vec<&[(String, Node)]> = items
        .map(|n| match n {
            Node::Obj(f) if !f.is_empty() => Some(f.as_slice()),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let first = objects.first()?;
    let same_keys = objects.iter().all(|o| {
        o.len() == first.len() && first.iter().all(|(k, _)| o.iter().any(|(k2, _)| k2 == k))
    });
    if !same_keys {
        return None;
    }
    first
        .iter()
        .map(|(name, _)| {
            let column: Vec<&Node> = objects.iter().filter_map(|o| lookup(o, name)).collect();
            if column.iter().all(|n| is_primitive(n)) {
                Some(Col {
                    name: name.clone(),
                    sub: Vec::new(),
                })
            } else {
                let sub = columns(column.into_iter())?;
                Some(Col {
                    name: name.clone(),
                    sub,
                })
            }
        })
        .collect()
}

/// Columns for an object with at least two entries whose values form a table.
fn keyed_columns(fields: &[(String, Node)]) -> Option<Vec<Col>> {
    if fields.len() < 2 {
        return None;
    }
    columns(fields.iter().map(|(_, v)| v))
}

fn lookup<'a>(fields: &'a [(String, Node)], name: &str) -> Option<&'a Node> {
    fields.iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

fn field_list(cols: &[Col]) -> String {
    let names: Vec<String> = cols
        .iter()
        .map(|c| match c.sub.is_empty() {
            true => key(&c.name),
            false => format!("{}{{{}}}", key(&c.name), field_list(&c.sub)),
        })
        .collect();
    names.join(",")
}

/// One row's cells in depth-first header order.
fn cells(row: &Node, cols: &[Col]) -> Vec<String> {
    let Node::Obj(fields) = row else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for col in cols {
        let value = lookup(fields, &col.name).unwrap_or(&Node::Null);
        if col.sub.is_empty() {
            out.push(primitive(value));
        } else {
            out.extend(cells(value, &col.sub));
        }
    }
    out
}
