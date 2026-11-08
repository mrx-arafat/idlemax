//! Delivery: the key-ordered JSON-model tree the TOON encoder reads.
//! `serde_json::Value` sorts keys, which would reorder table columns, so output is built
//! from this tree instead; it reads JSON text in document order.

use std::fmt;

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Null,
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    Str(String),
    Arr(Vec<Node>),
    Obj(Vec<(String, Node)>),
}

impl Node {
    /// Any serializable value, with struct fields in declaration order.
    pub fn of<T: Serialize>(value: &T) -> Result<Node, serde_json::Error> {
        serde_json::from_str(&serde_json::to_string(value)?)
    }

    pub fn str(s: impl Into<String>) -> Node {
        Node::Str(s.into())
    }

    pub fn obj<K: Into<String>>(fields: impl IntoIterator<Item = (K, Node)>) -> Node {
        Node::Obj(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }
}

impl From<u64> for Node {
    fn from(n: u64) -> Self {
        Node::Uint(n)
    }
}

impl From<i64> for Node {
    fn from(n: i64) -> Self {
        Node::Int(n)
    }
}

impl From<bool> for Node {
    fn from(b: bool) -> Self {
        Node::Bool(b)
    }
}

impl From<&str> for Node {
    fn from(s: &str) -> Self {
        Node::Str(s.to_string())
    }
}

impl From<String> for Node {
    fn from(s: String) -> Self {
        Node::Str(s)
    }
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(NodeVisitor)
    }
}

struct NodeVisitor;

impl<'de> Visitor<'de> for NodeVisitor {
    type Value = Node;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_unit<E>(self) -> Result<Node, E> {
        Ok(Node::Null)
    }

    fn visit_bool<E>(self, v: bool) -> Result<Node, E> {
        Ok(Node::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Node, E> {
        Ok(Node::Int(v))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Node, E> {
        Ok(Node::Uint(v))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Node, E> {
        Ok(Node::Float(v))
    }

    fn visit_str<E>(self, v: &str) -> Result<Node, E> {
        Ok(Node::Str(v.to_string()))
    }

    fn visit_string<E>(self, v: String) -> Result<Node, E> {
        Ok(Node::Str(v))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Node, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Node::Arr(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
        let mut fields = Vec::new();
        while let Some(entry) = map.next_entry::<String, Node>()? {
            fields.push(entry);
        }
        Ok(Node::Obj(fields))
    }
}
