//! Tests: the encoder against the TOON 4.4 spec's own examples (Appendix A) and its rules.

use super::{encode, Node};

/// Encodes JSON text, keeping the key order it was written in.
fn enc(json: &str) -> String {
    encode(&serde_json::from_str::<Node>(json).expect("test JSON"))
}

#[test]
fn objects_nest_by_indentation() {
    assert_eq!(
        enc(r#"{"id":123,"name":"Ada","active":true}"#),
        "id: 123\nname: Ada\nactive: true"
    );
    assert_eq!(
        enc(r#"{"user":{"id":123,"name":"Ada"}}"#),
        "user:\n  id: 123\n  name: Ada"
    );
    assert_eq!(enc("{}"), "");
    assert_eq!(enc(r#"{"o":{}}"#), "o:");
}

#[test]
fn primitive_arrays_are_inline_and_empty_arrays_are_brackets() {
    assert_eq!(
        enc(r#"{"tags":["admin","ops","dev"]}"#),
        "tags[3]: admin,ops,dev"
    );
    assert_eq!(enc(r#"{"tags":[]}"#), "tags: []");
    assert_eq!(enc("[]"), "[]");
    assert_eq!(enc("[1,2]"), "[2]: 1,2");
}

#[test]
fn arrays_of_arrays_use_list_form() {
    assert_eq!(
        enc(r#"{"pairs":[[1,2],[3,4]]}"#),
        "pairs[2]:\n  - [2]: 1,2\n  - [2]: 3,4"
    );
    assert_eq!(enc(r#"{"p":[[]]}"#), "p[1]:\n  - [0]:");
}

#[test]
fn uniform_objects_become_a_table() {
    assert_eq!(
        enc(r#"{"items":[{"sku":"A1","qty":2,"price":9.99},{"sku":"B2","qty":1,"price":14.5}]}"#),
        "items[2]{sku,qty,price}:\n  A1,2,9.99\n  B2,1,14.5"
    );
}

#[test]
fn table_columns_follow_the_first_row_whatever_the_key_order_of_the_rest() {
    assert_eq!(
        enc(r#"[{"b":1,"a":2},{"a":4,"b":3}]"#),
        "[2]{b,a}:\n  1,2\n  3,4"
    );
}

#[test]
fn nested_uniform_objects_collapse_into_field_groups() {
    assert_eq!(
        enc(
            r#"{"orders":[{"id":1,"customer":{"name":"Ada","country":"DK"},"total":99},
                           {"id":2,"customer":{"name":"Bob","country":"UK"},"total":149}]}"#
        ),
        "orders[2]{id,customer{name,country},total}:\n  1,Ada,DK,99\n  2,Bob,UK,149"
    );
}

#[test]
fn objects_of_uniform_objects_use_the_keyed_table() {
    let json = r#"{"users":{"alice":{"age":30,"city":"Berlin"},"bob":{"age":25,"city":"Oslo"}}}"#;
    assert_eq!(
        enc(json),
        "users[2:]{age,city}:\n  alice: 30,Berlin\n  bob: 25,Oslo"
    );
    assert_eq!(
        enc(r#"{"alice":{"age":30,"city":"Berlin"},"bob":{"age":25,"city":"Oslo"}}"#),
        "[2:]{age,city}:\n  alice: 30,Berlin\n  bob: 25,Oslo"
    );
    // One entry is not enough: it nests.
    assert_eq!(
        enc(r#"{"u":{"alice":{"age":30}}}"#),
        "u:\n  alice:\n    age: 30"
    );
}

#[test]
fn mixed_arrays_use_list_form_and_objects_put_their_first_field_on_the_hyphen() {
    assert_eq!(
        enc(r#"{"items":[1,{"a":1},"text"]}"#),
        "items[3]:\n  - 1\n  - a: 1\n  - text"
    );
    assert_eq!(
        enc(r#"{"items":[{"id":1,"name":"First"},{"id":2,"name":"Second","extra":true}]}"#),
        "items[2]:\n  - id: 1\n    name: First\n  - id: 2\n    name: Second\n    extra: true"
    );
    assert_eq!(enc(r#"{"items":[{},1]}"#), "items[2]:\n  -\n  - 1");
}

#[test]
fn a_table_inside_a_list_item_sits_two_levels_below_the_hyphen() {
    assert_eq!(
        enc(
            r#"{"items":[{"users":[{"id":1,"name":"Ada"},{"id":2,"name":"Bob"}],"status":"active"}]}"#
        ),
        "items[1]:\n  - users[2]{id,name}:\n      1,Ada\n      2,Bob\n    status: active"
    );
}

#[test]
fn strings_are_quoted_only_when_ambiguous() {
    assert_eq!(
        enc(
            r#"{"links":[{"id":1,"url":"http://a:b"},{"id":2,"url":"https://example.com?q=a:b"}]}"#
        ),
        "links[2]{id,url}:\n  1,\"http://a:b\"\n  2,\"https://example.com?q=a:b\""
    );
    let quoted = [
        ("", r#""""#),
        (" pad", r#"" pad""#),
        ("pad ", r#""pad ""#),
        ("true", r#""true""#),
        ("null", r#""null""#),
        ("42", r#""42""#),
        ("-3.14", r#""-3.14""#),
        ("05", r#""05""#),
        ("1e-6", r#""1e-6""#),
        ("a,b", r#""a,b""#),
        ("a:b", r#""a:b""#),
        ("say \"hi\"", r#""say \"hi\"""#),
        ("back\\slash", r#""back\\slash""#),
        ("[x]", r#""[x]""#),
        ("{x}", r#""{x}""#),
        ("-dash", r#""-dash""#),
        ("#tag", r##""#tag""##),
        ("line\nbreak", r#""line\nbreak""#),
        ("tab\there", r#""tab\there""#),
        ("cr\rx", r#""cr\rx""#),
        ("bell\u{7}", r#""bell\u0007""#),
    ];
    for (value, want) in quoted {
        let json = serde_json::to_string(&serde_json::json!({ "k": value })).unwrap();
        assert_eq!(enc(&json), format!("k: {want}"), "{value:?}");
    }
    let plain = [
        "Ada",
        "two words",
        "Hello 世界 👋",
        "a-b",
        "TRUE",
        "1.2.3",
        "v1",
        "x.y",
    ];
    for value in plain {
        let json = serde_json::to_string(&serde_json::json!({ "k": value })).unwrap();
        assert_eq!(enc(&json), format!("k: {value}"), "{value:?}");
    }
}

#[test]
fn keys_are_quoted_unless_they_are_identifiers() {
    assert_eq!(enc(r#"{"my-key":[1,2,3]}"#), "\"my-key\"[3]: 1,2,3");
    assert_eq!(
        enc(r#"{"x-items":[{"id":1,"name":"Ada"},{"id":2,"name":"Bob"}]}"#),
        "\"x-items\"[2]{id,name}:\n  1,Ada\n  2,Bob"
    );
    assert_eq!(
        enc(r#"{"x-items":[{"id":1},{"id":2,"label":"archived"}]}"#),
        "\"x-items\"[2]:\n  - id: 1\n  - id: 2\n    label: archived"
    );
    assert_eq!(
        enc(r#"{"user.name":1,"_a1":2,"1a":3,"":4}"#),
        "user.name: 1\n_a1: 2\n\"1a\": 3\n\"\": 4"
    );
}

#[test]
fn numbers_use_the_canonical_decimal_form() {
    let cases = [
        ("9007199254740992", "9007199254740992"),
        ("0.3333333333333333", "0.3333333333333333"),
        ("1.0", "1"),
        ("1.5000", "1.5"),
        ("-0.0", "0"),
        ("1e6", "1000000"),
        ("1e-6", "0.000001"),
        ("1e-7", "1e-7"),
        ("1e21", "1e+21"),
        ("-12", "-12"),
        ("18446744073709551615", "18446744073709551615"),
    ];
    for (json, want) in cases {
        assert_eq!(
            enc(&format!(r#"{{"n":{json}}}"#)),
            format!("n: {want}"),
            "{json}"
        );
    }
    assert_eq!(enc(r#"{"a":null,"b":false}"#), "a: null\nb: false");
}

#[test]
fn no_trailing_newline_or_spaces() {
    let out = enc(r#"{"a":{"b":{"c":[1,2]}},"d":[{"x":1,"y":[1]},{"x":2,"y":[]}]}"#);
    assert!(!out.ends_with('\n'));
    assert!(out.lines().all(|l| l == l.trim_end()), "{out:?}");
}
