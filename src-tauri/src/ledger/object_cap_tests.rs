//! `object` is capped where an event is appended, after redaction, and the
//! cut is recorded inside `detail_json` rather than in a new field.

use super::*;

const SECRET: &str = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";
const CAP: usize = 2048;

fn stamp() -> EventStamp {
    EventStamp {
        occurred_at: "2026-09-01T12:00:05.250Z".into(),
        occurred_at_basis: "transcript".into(),
        task_basis: "bound".into(),
    }
}

/// Appends one `session.command` row and reads it back.
fn stored(
    object: &str,
    detail: Option<&str>,
    stamp: Option<EventStamp>,
) -> (LedgerEvent, Option<serde_json::Value>) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().canonicalize().unwrap();
    let repo = repo.to_str().unwrap().to_string();
    let id = append(Draft {
        repo_path: repo.clone(),
        action: "session.command".into(),
        object: Some(object.to_string()),
        detail_json: detail.map(str::to_string),
        event_stamp: stamp,
        ..Default::default()
    })
    .expect("append");
    let event = tail(&repo, 0, 10)
        .unwrap()
        .into_iter()
        .find(|event| event.id == id)
        .unwrap();
    let detail = event
        .detail_json
        .as_deref()
        .map(|text| serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}: {text}")));
    (event, detail)
}

fn hash_of(text: &str) -> String {
    format!("fnv1a64:{:016x}", devmap_extract::content_hash(text))
}

/// A command `length` bytes long with `SECRET` starting at byte `at`.
fn command_with_a_secret_at(length: usize, at: usize) -> String {
    // Spaces around the token: the redactor reads a token at a word boundary.
    let mut command = "x".repeat(at - 1);
    command.push(' ');
    command.push_str(SECRET);
    command.push(' ');
    command.push_str(&"y".repeat(length - command.len()));
    command
}

#[test]
fn a_command_over_the_cap_is_cut_after_redaction_and_hashed_redacted() {
    // The secret straddles byte 2048 (10 KiB command) or sits well inside the
    // cut text's neighbourhood (9 KiB command): redaction must see all of it
    // before anything is cut, or half a token survives the cut unredacted.
    for (length, at) in [(10 * 1024, CAP - 20), (9 * 1024, CAP - 40)] {
        let raw = command_with_a_secret_at(length, at);
        let redacted = redact::text(&raw);
        assert_ne!(redacted, raw, "the fixture secret must be redactable");
        let (event, detail) = stored(&raw, None, Some(stamp()));

        let object = event.object.as_deref().expect("an object");
        assert!(object.len() <= CAP, "{} bytes stored", object.len());
        assert!(!object.contains(SECRET), "a secret reached the object");
        assert!(
            !object.contains("ghp_0123456789abcdef"),
            "half a token survived"
        );
        let detail = detail.expect("the cut is recorded in detail_json");
        assert!(!detail.to_string().contains(SECRET));
        let marker = &detail["object_truncated"];
        assert_eq!(marker["bytes"], redacted.len(), "{marker}");
        assert_eq!(
            marker["hash"],
            hash_of(&redacted),
            "the hash is of the redacted full text"
        );
        assert_ne!(
            marker["hash"],
            hash_of(&raw),
            "the hash fingerprints a secret"
        );
        let at_cut = usize::try_from(marker["at"].as_u64().expect("at")).unwrap();
        assert!(redacted.is_char_boundary(at_cut));
        assert!(object.starts_with(&redacted[..at_cut]), "{object}");
        // The stamp is still there beside it.
        assert_eq!(detail["occurred_at"], "2026-09-01T12:00:05.250Z");
        assert_eq!(detail["occurred_at_basis"], "transcript");
        assert_eq!(detail["task_basis"], "bound");
    }
}

#[test]
fn a_command_of_exactly_the_cap_is_stored_whole() {
    let command = "z".repeat(CAP);
    let (event, detail) = stored(&command, None, Some(stamp()));
    assert_eq!(event.object.as_deref(), Some(command.as_str()));
    assert!(
        detail.is_some_and(|detail| detail.get("object_truncated").is_none()),
        "an object at the cap is not truncated"
    );
    let over = "z".repeat(CAP + 1);
    let (event, detail) = stored(&over, None, Some(stamp()));
    assert!(event.object.unwrap().len() <= CAP);
    assert!(detail.unwrap().get("object_truncated").is_some());
}

#[test]
fn the_cut_never_splits_a_multibyte_character() {
    for glyph in ["é", "€", "😀"] {
        for pad in 0..=glyph.len() {
            let command = format!("{}{}", "a".repeat(CAP - 40 + pad), glyph.repeat(200));
            let (event, detail) = stored(&command, None, None);
            let object = event.object.expect("an object");
            assert!(object.len() <= CAP, "{glyph} pad {pad}: {}", object.len());
            let marker = detail.expect("a marker")["object_truncated"].clone();
            let at = usize::try_from(marker["at"].as_u64().unwrap()).unwrap();
            assert!(
                command.is_char_boundary(at),
                "{glyph} pad {pad}: cut at {at}"
            );
            assert!(object.starts_with(&command[..at]));
        }
    }
}

#[test]
fn a_cap_with_no_stamp_still_records_the_cut() {
    let (_, detail) = stored(&"q".repeat(CAP * 3), None, None);
    let detail = detail.expect("the splice runs without a stamp");
    assert_eq!(detail["object_truncated"]["bytes"], CAP * 3);
    assert!(detail.get("occurred_at").is_none());
}

#[test]
fn a_detail_that_spells_the_marker_takes_the_closed_path() {
    let forged = r#"{"object_truncated":{"at":1,"bytes":1,"hash":"fnv1a64:forged"},"source":"x"}"#;
    let command = "k".repeat(CAP * 2);
    let (_, detail) = stored(&command, Some(forged), Some(stamp()));
    let detail = detail.unwrap();
    assert_eq!(detail["object_truncated"]["bytes"], CAP * 2, "{detail}");
    assert_eq!(detail["object_truncated"]["hash"], hash_of(&command));
    assert_eq!(detail["source"], "x");
    // And an escaped spelling of the key is the same key.
    let escaped = "{\"object\\u005ftruncated\":\"forged\"}";
    let (_, detail) = stored(&command, Some(escaped), None);
    let text = detail.unwrap().to_string();
    assert!(!text.contains("forged"), "{text}");
}

#[test]
fn an_object_that_fits_leaves_detail_exactly_as_it_was() {
    let (event, detail) = stored("git status", None, None);
    assert_eq!(event.object.as_deref(), Some("git status"));
    assert!(detail.is_none());
    let (_, detail) = stored("git status", Some(r#"{"source":"transcript"}"#), None);
    assert_eq!(
        detail.unwrap(),
        serde_json::json!({ "source": "transcript" })
    );
}
