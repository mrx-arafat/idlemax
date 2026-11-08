//! Tests: JobSpec idempotency key rules and wire compatibility.

use super::*;

fn spec() -> JobSpec {
    JobSpec {
        name: "tests".into(),
        command: vec!["cargo".into(), "test".into()],
        ..JobSpec::default()
    }
}

#[test]
fn a_key_must_be_present_and_short() {
    let with = |key: &str| JobSpec {
        key: Some(key.into()),
        ..spec()
    };
    assert_eq!(with("pr-42-tests").validate(), Ok(()));
    assert_eq!(with("  ").validate(), Err(SpecError::EmptyKey));
    assert_eq!(
        with(&"k".repeat(MAX_KEY_CHARS + 1)).validate(),
        Err(SpecError::KeyTooLong)
    );
    assert_eq!(with(&"k".repeat(MAX_KEY_CHARS)).validate(), Ok(()));
}

#[test]
fn specs_without_a_key_round_trip_unchanged_for_old_peers() {
    let json = serde_json::to_value(spec()).unwrap();
    assert!(
        json.get("key").is_none(),
        "an unset key is not sent: {json}"
    );

    let old: JobSpec = serde_json::from_str(r#"{"name":"n","command":["true"]}"#).unwrap();
    assert_eq!(old.key, None);
}
