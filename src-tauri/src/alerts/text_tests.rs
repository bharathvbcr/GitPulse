//! The notification text cleaner against `scripts/fixtures/sanitize-cases.json`,
//! the table `sessionActivity.test.ts` reads too, so the two sides cannot
//! disagree about a row without one of them failing.

use super::scan::{Channel, Scanner};
use super::text::{is_hidden, printable, sanitize};
use super::{bridge, Notice};
use serde_json::Value;

struct Row {
    name: String,
    input: String,
    clean: String,
    printable: String,
    wire: bool,
}

fn rows() -> Vec<Row> {
    let table: Value = serde_json::from_str(include_str!(
        "../../../scripts/fixtures/sanitize-cases.json"
    ))
    .expect("the fixture is JSON");
    let rows: Vec<Row> = table["cases"]
        .as_array()
        .expect("the fixture has cases")
        .iter()
        .map(|row| {
            let text = |key: &str| row[key].as_str().expect(key).to_owned();
            Row {
                name: text("name"),
                input: text("input"),
                clean: text("clean"),
                printable: text("printable"),
                wire: row["wire"].as_bool().expect("wire"),
            }
        })
        .collect();
    assert!(rows.len() >= 15, "the table lost rows: {}", rows.len());
    rows
}

#[test]
fn every_row_cleans_to_its_expected_line() {
    for row in rows() {
        assert_eq!(sanitize(&row.input), row.clean, "sanitize: {}", row.name);
        assert_eq!(
            printable(&row.input, 10_000),
            row.printable,
            "printable: {}",
            row.name
        );
    }
}

#[test]
fn the_table_names_every_required_class() {
    // The rows are the evidence; this keeps one from being deleted quietly.
    let all: String = rows().iter().map(|row| row.input.as_str()).collect();
    for (label, probe) in [
        ("U+202E", '\u{202e}'),
        ("U+2066", '\u{2066}'),
        ("U+2069", '\u{2069}'),
        ("U+200B", '\u{200b}'),
        ("U+200F", '\u{200f}'),
        ("U+FEFF", '\u{feff}'),
        ("a tag", '\u{e0001}'),
        ("private use", '\u{e000}'),
    ] {
        assert!(all.contains(probe), "no row exercises {label}");
        assert!(is_hidden(probe), "{label} is not hidden");
    }
}

/// The same text arriving by each channel an agent or a peer has: the three
/// OSC conventions the scanner reads, and the `message` a hook builds from
/// `last_assistant_message`. All of them end at one cleaner.
#[test]
fn every_channel_cleans_a_row_the_same_way() {
    use base64::Engine as _;
    for row in rows().into_iter().filter(|row| row.wire) {
        let name = &row.name;
        let expect = (!row.clean.is_empty()).then_some(row.clean.as_str());

        let osc9 = Scanner::new().feed(format!("\x1b]9;{}\x07", row.input).as_bytes());
        assert_eq!(osc9.len(), 1, "OSC 9: {name}");
        assert_eq!(osc9[0].channel, Channel::Osc9, "{name}");
        assert_eq!(osc9[0].body.as_deref(), expect, "OSC 9: {name}");

        let osc777 = Scanner::new().feed(format!("\x1b]777;notify;T;{}\x07", row.input).as_bytes());
        assert_eq!(osc777.len(), 1, "OSC 777: {name}");
        assert_eq!(osc777[0].body.as_deref(), expect, "OSC 777: {name}");

        let encoded = base64::engine::general_purpose::STANDARD.encode(&row.input);
        let osc99 =
            Scanner::new().feed(format!("\x1b]99;i=1:p=title:e=1;{encoded}\x1b\\").as_bytes());
        assert_eq!(osc99.len(), 1, "OSC 99: {name}");
        assert_eq!(osc99[0].channel, Channel::Osc99, "{name}");
        assert_eq!(osc99[0].title.as_deref(), expect, "OSC 99: {name}");

        let report = serde_json::json!({
            "v": 1, "event": "turn_finished", "session": "term-1-a", "message": row.input
        })
        .to_string();
        let notice = bridge::parse_report(report.as_bytes()).expect("a valid report");
        assert_eq!(notice.detail.as_deref(), expect, "hook message: {name}");
    }
}

/// GitPulse's own heading is built from the working directory's name, which a
/// program chooses. An override in it would reorder the heading, so the name
/// the user reads as `repo · Claude Code` could read as another string.
#[test]
fn a_cwd_with_an_override_renders_its_heading_in_logical_order() {
    let report = serde_json::json!({
        "v": 1, "event": "permission_prompt", "agent": "claude",
        "session": "term-9-1a", "cwd": "/Users/me/pro\u{202e}gnp.exe\u{2066}"
    })
    .to_string();
    let notice = bridge::parse_report(report.as_bytes()).expect("a valid report");
    assert_eq!(notice.place.as_deref(), Some("prognp.exe"));
    let heading = notice.heading();
    assert_eq!(heading, "Claude Code · prognp.exe");
    assert!(!heading.chars().any(is_hidden), "{heading:?}");
}

/// A place that reached a notice by another door than `parse_report` (a
/// terminal's own title) is held to the same rule when the heading is built.
#[test]
fn a_heading_is_cleaned_whatever_door_its_place_came_through() {
    let mut notice: Notice =
        bridge::parse_report(br#"{"v":1,"event":"permission_prompt","session":"term-9-1a"}"#)
            .expect("a valid report");
    notice.place = Some("work\u{202e}\u{200b}tree\n2".into());
    assert_eq!(notice.heading(), "Claude Code · worktree 2");
}
