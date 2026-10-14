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

// ---- adversarial cases -------------------------------------------------

/// A detail naming a stamp key through a JSON unicode escape is the same key to
/// every JSON parser, and `names_a_key` (a substring test on the raw text) does
/// not see it. The row then holds the key twice, and which value a reader gets
/// depends on the reader: serde and JS take the last, SQLite's `json_extract`
/// takes the first.
#[test]
fn an_escaped_spelling_of_a_stamp_key_is_not_given_twice() {
    let detail = r#"{"occurred_at":"FORGED","task_basis":"FORGED","occurred_at_basis":"FORGED"}"#;
    eprintln!("spliced: {:?}", redact_detail(Some(detail), Some(&stamp())));
    let repo = tempfile::tempdir().unwrap();
    let repo = repo.path().canonicalize().unwrap();
    let repo = repo.to_str().unwrap().to_string();
    let id = append(Draft {
        repo_path: repo.clone(),
        action: "session.edit".into(),
        detail_json: Some(detail.into()),
        event_stamp: Some(stamp()),
        ..Default::default()
    })
    .unwrap();
    let first_wins: String = with_conn(&repo, |conn| {
        conn.query_row(
            "SELECT json_extract(detail_json, '$.occurred_at') FROM events WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .map_err(|e| LedgerError::new("q", e.to_string()))
    })
    .unwrap();
    assert_eq!(
        first_wins, "2026-09-01T12:00:05.250Z",
        "SQL readers see the caller's value, not the stamp"
    );
}

/// Anything that is not a JSON object carries no stamp, takes no splice, and
/// never loses redaction or the row.
#[test]
fn a_detail_that_is_not_an_object_never_panics_leaks_or_drops_the_row() {
    let secret = format!("[\"{KEY}\"]");
    let string = format!("\"{KEY}\"");
    let broken = format!("{{\"a\":\"{KEY}\"");
    let trailing = format!("{{\"a\":1}} {{\"b\":\"{KEY}\"}}");
    let cases: Vec<&str> = vec![
        "", "null", "[]", "[1]", "\"s\"", "{", "}", "}{", "{}{}", "  {} ", "\u{feff}{}", &secret,
        &string, &broken, &trailing,
    ];
    for detail in cases {
        let repo = tempfile::tempdir().unwrap();
        let repo = repo.path().canonicalize().unwrap();
        let repo = repo.to_str().unwrap().to_string();
        let id = append(Draft {
            repo_path: repo.clone(),
            action: "session.edit".into(),
            detail_json: Some(detail.into()),
            event_stamp: Some(stamp()),
            ..Default::default()
        })
        .unwrap_or_else(|e| panic!("{detail:?} dropped the row: {e}"));
        let stored = tail(&repo, 0, 10)
            .unwrap()
            .into_iter()
            .find(|event| event.id == id)
            .unwrap()
            .detail_json
            .unwrap_or_default();
        assert!(!stored.contains(KEY), "{detail:?} leaked: {stored}");
    }
}

/// A stamp on a valid-JSON object detail must come out as valid JSON for every
/// shape of object, including ones the redactor rewrites.
#[test]
fn the_splice_always_yields_valid_json_with_the_stamp_for_an_object_detail() {
    let nested_deep = (0..100).fold(format!("\"{KEY}\""), |inner, _| format!("{{\"k\":{inner}}}"));
    let details = [
        "{}".to_string(),
        "{ }".to_string(),
        "\n{\n}\n".to_string(),
        format!("{{\"{KEY}\":1,\"{KEY}x\":2}}"),
        format!("{{\"a\":\"}}\",\"b\":\"{KEY}\"}}"),
        format!("{{\"deep\":{nested_deep}}}"),
        "{\"a\":\"\\\",\\\"occurred_at\\\":\\\"x\"}".to_string(),
    ];
    for detail in details {
        let out = redact_detail(Some(&detail), Some(&stamp())).unwrap();
        assert!(
            !out.contains(KEY),
            "leaked from {}: {out}",
            &detail[..detail.len().min(80)]
        );
        let parsed: serde_json::Value = serde_json::from_str(&out)
            .unwrap_or_else(|e| panic!("invalid JSON ({e}) from {detail}: {out}"));
        assert_eq!(parsed["occurred_at"], "2026-09-01T12:00:05.250Z", "{out}");
        assert_eq!(parsed["task_basis"], "bound", "{out}");
    }
}

/// Nesting beyond the redactor's structural depth falls back to text
/// redaction. The secret must still not reach the splice's output.
#[test]
fn a_detail_nested_past_the_redactor_depth_still_does_not_leak() {
    for depth in [150usize, 300, 5_000] {
        let nested = (0..depth).fold(format!("\"{KEY}\""), |inner, _| format!("{{\"k\":{inner}}}"));
        let detail = format!("{{\"deep\":{nested}}}");
        let out = redact_detail(Some(&detail), Some(&stamp())).unwrap();
        assert!(!out.contains(KEY), "depth {depth} leaked");
        assert!(out.ends_with(r#""task_basis":"bound"}"#), "depth {depth}");
    }
}

/// A large detail with a stamp: the splice is linear and loses nothing.
#[test]
fn a_very_large_detail_still_takes_the_stamp_and_stays_redacted() {
    let big = "x".repeat(8 * 1024 * 1024);
    let detail = serde_json::json!({ "blob": big, "secret": KEY }).to_string();
    let started = std::time::Instant::now();
    let out = redact_detail(Some(&detail), Some(&stamp())).unwrap();
    assert!(!out.contains(KEY));
    assert!(out.ends_with(r#""task_basis":"bound"}"#));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
}

/// `is_well_formed` is the only thing between the splice and the disk. Its
/// shape check accepts any digits, so a Draft built by another caller can store
/// a "timestamp" that is no instant at all. Ingest formats from milliseconds
/// and cannot produce one; this pins what the gate itself claims.
#[test]
fn the_well_formed_check_demands_a_real_calendar_instant() {
    for impossible in [
        "9999-99-99T99:99:99.999Z",
        "2026-13-40T25:61:61.000Z",
        "0000-00-00T00:00:00.000Z",
    ] {
        let bad = EventStamp {
            occurred_at: impossible.into(),
            ..stamp()
        };
        assert!(!bad.is_well_formed(), "{impossible} is not an instant");
    }
}
