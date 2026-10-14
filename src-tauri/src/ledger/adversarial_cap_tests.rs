//! Adversarial probes of the ledger `object` cap (card 10). A failing test is a
//! demonstrated defect; a passing one is kept as a regression pin.

use super::*;

const SECRET: &str = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";
const CAP: usize = 2048;

fn repo_dir() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir
        .path()
        .canonicalize()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    (dir, repo)
}

fn append_row(repo: &str, object: &str, detail: Option<&str>) -> LedgerEvent {
    let id = append(Draft {
        repo_path: repo.to_string(),
        action: "session.command".into(),
        object: Some(object.to_string()),
        detail_json: detail.map(str::to_string),
        ..Default::default()
    })
    .expect("append");
    tail(repo, 0, 100)
        .unwrap()
        .into_iter()
        .find(|event| event.id == id)
        .unwrap()
}

fn hash_of(text: &str) -> String {
    format!("fnv1a64:{:016x}", devmap_extract::content_hash(text))
}

fn legacy_event(object: &str) -> LedgerEvent {
    LedgerEvent {
        id: 1,
        ulid: "01J00000000000000000000001".into(),
        ts_utc: "2026-10-14T00:00:00.000Z".into(),
        schema_version: SCHEMA_VERSION,
        repo_path: "/r".into(),
        worktree_path: None,
        actor_kind: "agent".into(),
        actor_id: None,
        session_id: None,
        task_id: None,
        action: "session.command".into(),
        object: Some(object.into()),
        argv_json: None,
        outcome: "ok".into(),
        verdict_json: None,
        before_ref: None,
        after_ref: None,
        duration_ms: None,
        detail_json: None,
    }
}

/// Attack 7: every writer must cap. The family import copies rows from a
/// sibling worktree's ledger (which predates the cap) into the anchor.
#[test]
fn a_legacy_sibling_row_imported_into_the_anchor_is_capped() {
    let source = PathBuf::from("/source");
    let anchor = PathBuf::from("/anchor");
    let event = normalize_legacy_event(legacy_event(&"o".repeat(50_000)), &anchor, &source, &[])
        .expect("normalized");
    let object = event.object.unwrap();
    assert!(
        object.len() <= CAP,
        "family import stores a {} byte object, past the {CAP} byte cap",
        object.len()
    );
}

/// Attack 5: a caller-supplied detail must not be able to claim an `object`
/// cut that never happened.
#[test]
fn a_detail_cannot_forge_an_object_cut_marker_on_an_uncut_object() {
    let (_dir, repo) = repo_dir();
    let forged = r#"{"object_truncated":{"at":3,"bytes":99999,"hash":"fnv1a64:0000000000000000"}}"#;
    let event = append_row(&repo, "git status", Some(forged));
    assert_eq!(event.object.as_deref(), Some("git status"));
    let detail = event.detail_json.unwrap_or_default();
    assert!(
        !detail.contains("object_truncated"),
        "the object was not cut but the row claims it was: {detail}"
    );
}

/// Attack 5/7: a cut with a detail that is not a JSON object must still record
/// the cut, or the stored object ends in the suffix with no hash and no length.
#[test]
fn a_cut_object_with_a_non_object_detail_still_records_the_cut() {
    for detail in ["plain text", "[1,2,3]", "\"s\""] {
        let (_dir, repo) = repo_dir();
        let event = append_row(&repo, &"c".repeat(5000), Some(detail));
        let stored = event.object.unwrap();
        assert!(stored.ends_with("[truncated]"), "{stored}");
        let recorded = event.detail_json.unwrap_or_default();
        assert!(
            recorded.contains("object_truncated"),
            "detail {detail:?}: the object was cut but no marker was recorded: {recorded}"
        );
    }
}

/// Attack 5: redact first, hash the redacted text, never the raw one; and a
/// secret that straddles the cut point does not survive in either half.
#[test]
fn the_hash_covers_the_redacted_text_and_a_straddling_secret_is_gone() {
    for at in [CAP - 40, CAP - 20, CAP - 14, CAP - 1, CAP] {
        let (_dir, repo) = repo_dir();
        let mut command = "x".repeat(at - 1);
        command.push(' ');
        command.push_str(SECRET);
        command.push(' ');
        command.push_str(&"y".repeat(3000));
        let event = append_row(&repo, &command, None);
        let stored = event.object.unwrap();
        assert!(
            !stored.contains(&SECRET[4..16]),
            "at {at}: the secret body survived the cut"
        );
        let detail: serde_json::Value = serde_json::from_str(&event.detail_json.unwrap()).unwrap();
        let hash = detail["object_truncated"]["hash"].as_str().unwrap();
        assert_eq!(hash, hash_of(&redact::text(&command)), "at {at}");
        assert_ne!(
            hash,
            hash_of(&command),
            "at {at}: the hash fingerprints the raw secret"
        );
    }
}

/// Attack 5: exact sizes and multibyte characters on the cut.
#[test]
fn the_cut_is_exact_and_never_splits_a_code_point() {
    let (_dir, repo) = repo_dir();
    let at_cap = append_row(&repo, &"a".repeat(CAP), None);
    assert_eq!(at_cap.object.unwrap().len(), CAP);
    assert!(!at_cap
        .detail_json
        .unwrap_or_default()
        .contains("object_truncated"));
    for glyph in ["é", "€", "😀"] {
        for pad in 0..glyph.len() + 14 {
            for total in [CAP + 1, CAP + 2, CAP + 3, CAP + 4] {
                let (_d, repo) = repo_dir();
                let mut text = "a".repeat(CAP - 20 + pad);
                while text.len() < total {
                    text.push_str(glyph);
                }
                let event = append_row(&repo, &text, None);
                let stored = event.object.unwrap();
                assert!(stored.len() <= CAP, "{glyph} pad {pad}: {}", stored.len());
                assert!(stored.is_char_boundary(stored.len() - "… [truncated]".len()));
                assert!(event.detail_json.unwrap().contains("object_truncated"));
            }
        }
    }
}

/// Attack 5: a detail that spells the marker through an escape or in a long
/// object's row cannot choose the recorded hash.
#[test]
fn a_detail_spelling_the_marker_is_overwritten_by_the_real_one() {
    for detail in [
        r#"{"object_truncated":{"at":1,"bytes":1,"hash":"fnv1a64:ffffffffffffffff"}}"#,
        r#"{"object_truncated":{"at":1,"bytes":1,"hash":"fnv1a64:ffffffffffffffff"}}"#,
    ] {
        let (_dir, repo) = repo_dir();
        let text = "z".repeat(4000);
        let event = append_row(&repo, &text, Some(detail));
        let value: serde_json::Value = serde_json::from_str(&event.detail_json.unwrap()).unwrap();
        assert_eq!(
            value["object_truncated"]["hash"],
            hash_of(&text),
            "{detail}"
        );
        assert_eq!(value["object_truncated"]["bytes"], 4000);
    }
}

/// Attack 6: two distinct reflog identities that share the first 2 KiB must not
/// have the same stored form, or the second is judged "already recorded".
#[test]
fn two_long_identities_sharing_a_prefix_do_not_collide_once_stored() {
    let prefix = "m".repeat(3000);
    let first = format!("{} {prefix} first", "a".repeat(40));
    let second = format!("{} {prefix} second", "a".repeat(40));
    assert_ne!(first, second);
    assert_ne!(
        stored_object(&first),
        stored_object(&second),
        "the reflog dedupe would treat the second entry as already recorded and drop it"
    );
}

/// Attack 6: a reflog row stored before the cap existed (uncapped) must still
/// match its own identity, or every such row is recorded a second time.
#[test]
fn a_reflog_row_stored_uncapped_still_matches_its_identity() {
    let (_dir, repo) = repo_dir();
    append_row(&repo, "seed", None);
    let identity = format!("{} {}", "b".repeat(40), "w".repeat(5000));
    with_conn(&repo, |conn| {
        conn.execute(
            "INSERT INTO events (ulid, ts_utc, schema_version, repo_path, worktree_path,
                                 actor_kind, action, object, outcome)
             VALUES ('01J00000000000000000000009', '2026-10-14T00:00:00.000Z', ?1, ?2, ?2,
                     'system', 'reflog.commit', ?3, 'ok')",
            rusqlite::params![SCHEMA_VERSION, repo, redact::text(&identity)],
        )
        .map_err(|error| LedgerError::new("fixture_failed", error.to_string()))?;
        Ok(())
    })
    .unwrap();
    let seen = recorded_reflog_objects(&repo, &repo, true).unwrap();
    assert!(
        seen.contains(&stored_object(&identity))
            || seen.iter().any(|o| o == &stored_object(&identity)),
        "an uncapped pre-cap row is not matched by its capped identity: it will be re-recorded"
    );
}
