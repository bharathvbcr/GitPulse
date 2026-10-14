//! The event-time keys skip the redactor, and only those three, and only when
//! their values have the shape the writer produces.

use super::*;

const KEY: &str = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";

fn stamp() -> EventStamp {
    EventStamp {
        occurred_at: "2026-09-01T12:00:05.250Z".into(),
        occurred_at_basis: "transcript".into(),
        task_basis: "bound".into(),
    }
}

fn stored(detail: Option<&str>, stamp: EventStamp) -> serde_json::Value {
    let repo = tempfile::tempdir().unwrap();
    let repo = repo.path().canonicalize().unwrap();
    let repo = repo.to_str().unwrap().to_string();
    let id = append(Draft {
        repo_path: repo.clone(),
        action: "session.edit".into(),
        detail_json: detail.map(str::to_string),
        event_stamp: Some(stamp),
        ..Default::default()
    })
    .expect("a stamp that fails validation never drops the row");
    let event = tail(&repo, 0, 10)
        .unwrap()
        .into_iter()
        .find(|event| event.id == id)
        .unwrap();
    let text = event.detail_json.expect("a detail");
    assert!(!text.contains(KEY), "a secret reached the disk: {text}");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

#[test]
fn a_well_formed_stamp_is_spliced_onto_the_redacted_object() {
    assert!(stamp().is_well_formed());
    let out = redact_detail(
        Some(r#"{"source":"transcript","tool":"Edit"}"#),
        Some(&stamp()),
    );
    assert_eq!(
        out.as_deref(),
        Some(
            r#"{"source":"transcript","tool":"Edit","occurred_at":"2026-09-01T12:00:05.250Z","occurred_at_basis":"transcript","task_basis":"bound"}"#
        )
    );
    // No detail, and an empty one, carry the stamp alone.
    for detail in [None, Some("{}")] {
        let out = redact_detail(detail, Some(&stamp())).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["task_basis"], "bound", "{out}");
        assert_eq!(parsed.as_object().unwrap().len(), 3, "{out}");
    }
    // No stamp: the old path, byte for byte.
    assert_eq!(
        redact_detail(Some(r#"{"a":1}"#), None),
        Some(redact::text(r#"{"a":1}"#))
    );
}

#[test]
fn only_the_three_keys_bypass_redaction() {
    let detail = serde_json::json!({
        "source": "transcript",
        "note": KEY,
        "nested": { "token": KEY },
    })
    .to_string();
    let row = stored(Some(&detail), stamp());
    assert_eq!(row["occurred_at"], "2026-09-01T12:00:05.250Z");
    assert_eq!(row["occurred_at_basis"], "transcript");
    assert_eq!(row["task_basis"], "bound");
    assert_ne!(
        row["note"], KEY,
        "a secret in another key must still be redacted"
    );
    assert_ne!(row["nested"]["token"], KEY);
}

#[test]
fn a_secret_forced_into_a_stamp_key_fails_validation_and_is_redacted() {
    for forced in 0..3 {
        let mut bad = stamp();
        match forced {
            0 => bad.occurred_at = KEY.into(),
            1 => bad.occurred_at_basis = KEY.into(),
            _ => bad.task_basis = KEY.into(),
        }
        assert!(!bad.is_well_formed());
        // `stored` asserts the secret is not on disk and the row was written.
        let row = stored(Some(r#"{"source":"transcript"}"#), bad);
        assert_eq!(row["source"], "transcript");
        let key = ["occurred_at", "occurred_at_basis", "task_basis"][forced];
        assert_ne!(row[key], KEY, "{key}");
    }
}

#[test]
fn every_mismatch_takes_the_closed_path() {
    let good = stamp();
    let mismatches = [
        EventStamp {
            occurred_at: "2026-09-01".into(),
            ..good.clone()
        },
        EventStamp {
            occurred_at: "2026-09-01T12:00:05Z".into(),
            ..good.clone()
        },
        EventStamp {
            occurred_at: "2026-09-01T12:00:05.250+00:00".into(),
            ..good.clone()
        },
        EventStamp {
            occurred_at_basis: "guess".into(),
            ..good.clone()
        },
        EventStamp {
            task_basis: "Bound".into(),
            ..good.clone()
        },
        EventStamp {
            task_basis: "bound\",\"injected\":\"x".into(),
            ..good.clone()
        },
    ];
    for bad in mismatches {
        assert!(!bad.is_well_formed(), "{bad:?}");
        let out = redact_detail(Some(r#"{"source":"transcript"}"#), Some(&bad)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        // Through the object, not spliced as text: an injected quote stays inside its value.
        assert!(parsed.get("injected").is_none(), "{out}");
        assert_eq!(parsed["source"], "transcript");
        assert_eq!(parsed.as_object().unwrap().len(), 4, "{out}");
    }
}

#[test]
fn a_detail_that_already_names_a_key_is_not_given_it_twice() {
    let out = redact_detail(Some(r#"{"occurred_at":"x"}"#), Some(&stamp())).unwrap();
    assert_eq!(out.matches("occurred_at\"").count(), 1, "{out}");
    let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed["occurred_at"], "2026-09-01T12:00:05.250Z");
}
