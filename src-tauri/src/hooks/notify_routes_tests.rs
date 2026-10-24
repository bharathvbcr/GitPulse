//! The notify vocabulary guard: which host hook reports which event.
//!
//! `scripts/fixtures/notify-routes.json` lists every route. The shipped
//! manifest and the settings a launch injects are each compared to it as a
//! whole, and the routed event names are compared to `alerts::bridge::EVENTS`
//! in both directions. A count of entries cannot tell a deleted matcher from a
//! retargeted argument; set equality can, and the mutation tests below delete
//! and retarget every entry in turn to prove it.

use crate::alerts::bridge::EVENTS;
use serde_json::{json, Value};
use std::collections::BTreeSet;

fn table() -> Vec<(String, String, String, bool)> {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../scripts/fixtures/notify-routes.json"))
            .expect("the routes fixture is JSON");
    fixture["routes"]
        .as_array()
        .expect("routes")
        .iter()
        .map(|row| {
            (
                row["hook"].as_str().expect("hook").to_owned(),
                row["matcher"].as_str().unwrap_or_default().to_owned(),
                row["event"].as_str().expect("event").to_owned(),
                row["injected"].as_bool().expect("injected"),
            )
        })
        .collect()
}

fn manifest() -> Value {
    serde_json::from_str(include_str!("../../../plugins/gitpulse/hooks/hooks.json"))
        .expect("hooks.json is JSON")
}

fn injected() -> Value {
    crate::workbench::claude_hook::settings_hooks(std::path::Path::new(
        "/opt/gitpulse/gitpulse-hook",
    ))
    .expect("the injected settings build")
}

/// Every `(host hook, matcher, reported event)` among the `notify` handlers of
/// a `hooks` object, in the order met.
fn routes_of(hooks: &Value) -> Vec<(String, String, String)> {
    let mut found = Vec::new();
    for (hook, groups) in hooks.as_object().expect("a hooks object") {
        for group in groups.as_array().into_iter().flatten() {
            let matcher = group["matcher"].as_str().unwrap_or_default();
            for handler in group["hooks"].as_array().into_iter().flatten() {
                let command = handler["command"].as_str().unwrap_or_default();
                if let Some((_, event)) = command.split_once(" notify ") {
                    found.push((hook.clone(), matcher.to_owned(), event.trim().to_owned()));
                }
            }
        }
    }
    found.sort();
    found
}

/// What is wrong with `hooks` as the carrier of `expected` routes: empty when
/// nothing. `all_events` also demands the routed names equal `EVENTS`.
fn problems(hooks: &Value, expected: &[(String, String, String)], all_events: bool) -> Vec<String> {
    let mut out = Vec::new();
    let have = routes_of(hooks);
    let mut want = expected.to_vec();
    want.sort();
    for route in &want {
        if !have.contains(route) {
            out.push(format!("missing route {route:?}"));
        }
    }
    for route in &have {
        if !want.contains(route) {
            out.push(format!("unexpected route {route:?}"));
        }
    }
    if have.len() != want.len() && out.is_empty() {
        out.push("a route is registered twice".to_owned());
    }
    for (hook, matcher, event) in &have {
        if hook == "Notification" && matcher != event {
            out.push(format!("Notification matcher {matcher} reports {event}"));
        }
    }
    if all_events {
        let routed: BTreeSet<&str> = have.iter().map(|(_, _, event)| event.as_str()).collect();
        let known: BTreeSet<&str> = EVENTS.iter().map(|event| event.name).collect();
        for name in known.difference(&routed) {
            out.push(format!("EVENTS has {name}, which nothing routes"));
        }
        for name in routed.difference(&known) {
            out.push(format!("a route reports {name}, which EVENTS lacks"));
        }
    }
    out
}

fn expected(injected_only: bool) -> Vec<(String, String, String)> {
    table()
        .into_iter()
        .filter(|(_, _, _, inj)| *inj || !injected_only)
        .map(|(hook, matcher, event, _)| (hook, matcher, event))
        .collect()
}

#[test]
fn the_shipped_manifest_routes_exactly_the_events_the_socket_serves() {
    assert_eq!(
        problems(&manifest()["hooks"], &expected(false), true),
        Vec::<String>::new()
    );
}

#[test]
fn the_injected_settings_route_exactly_the_flagged_entries() {
    assert_eq!(
        problems(&injected(), &expected(true), false),
        Vec::<String>::new()
    );
}

/// `(hook, group index, handler index)` of every notify handler in `hooks`.
fn notify_positions(hooks: &Value) -> Vec<(String, usize, usize)> {
    let mut at = Vec::new();
    for (hook, groups) in hooks.as_object().unwrap() {
        for (g, group) in groups.as_array().unwrap().iter().enumerate() {
            for (h, handler) in group["hooks"].as_array().unwrap().iter().enumerate() {
                if handler["command"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(" notify ")
                {
                    at.push((hook.clone(), g, h));
                }
            }
        }
    }
    at
}

fn delete_entry(hooks: &mut Value, (hook, g, h): &(String, usize, usize)) {
    let handlers = hooks[hook.as_str()][*g]["hooks"].as_array_mut().unwrap();
    handlers.remove(*h);
    if handlers.is_empty() {
        hooks[hook.as_str()].as_array_mut().unwrap().remove(*g);
    }
}

fn retarget_entry(hooks: &mut Value, (hook, g, h): &(String, usize, usize), to: &str) {
    let command = hooks[hook.as_str()][*g]["hooks"][*h]["command"]
        .as_str()
        .unwrap()
        .to_owned();
    let (head, _) = command.split_once(" notify ").unwrap();
    hooks[hook.as_str()][*g]["hooks"][*h]["command"] = json!(format!("{head} notify {to}"));
}

/// Removing any one notify entry, from the manifest or from what a launch
/// injects, must fail: the mutation is made on a copy, never the shipped file.
#[test]
fn removing_any_notify_entry_fails_the_guard() {
    let shipped = manifest()["hooks"].clone();
    let positions = notify_positions(&shipped);
    assert_eq!(positions.len(), 13);
    for position in &positions {
        let mut copy = shipped.clone();
        delete_entry(&mut copy, position);
        assert!(
            !problems(&copy, &expected(false), true).is_empty(),
            "manifest: deleting {position:?} went unnoticed"
        );
    }
    let launch = injected();
    let positions = notify_positions(&launch);
    assert_eq!(positions.len(), 6);
    for position in &positions {
        let mut copy = launch.clone();
        delete_entry(&mut copy, position);
        assert!(
            !problems(&copy, &expected(true), false).is_empty(),
            "injected: deleting {position:?} went unnoticed"
        );
    }
}

/// Pointing any one notify entry at a different event must fail too, including
/// the retarget a count cannot see: SessionEnd reporting `turn_finished`.
#[test]
fn retargeting_any_notify_entry_fails_the_guard() {
    let shipped = manifest()["hooks"].clone();
    for position in notify_positions(&shipped) {
        for event in EVENTS {
            let mut copy = shipped.clone();
            retarget_entry(&mut copy, &position, event.name);
            if routes_of(&copy) == routes_of(&shipped) {
                continue;
            }
            assert!(
                !problems(&copy, &expected(false), true).is_empty(),
                "manifest: {position:?} retargeted to {} went unnoticed",
                event.name
            );
        }
    }
    let launch = injected();
    for position in notify_positions(&launch) {
        for event in EVENTS {
            let mut copy = launch.clone();
            retarget_entry(&mut copy, &position, event.name);
            if routes_of(&copy) == routes_of(&launch) {
                continue;
            }
            assert!(
                !problems(&copy, &expected(true), false).is_empty(),
                "injected: {position:?} retargeted to {} went unnoticed",
                event.name
            );
        }
    }
    let mut copy = shipped.clone();
    let session_end = notify_positions(&copy)
        .into_iter()
        .find(|(hook, _, _)| hook == "SessionEnd")
        .unwrap();
    retarget_entry(&mut copy, &session_end, "turn_finished");
    let found = problems(&copy, &expected(false), true);
    assert!(
        found.iter().any(|p| p.contains("session_ended")),
        "{found:?}"
    );
}

/// An event added to `EVENTS` with no route, or a route to a name `EVENTS`
/// lacks, is caught from either side even when the routes table is also edited
/// to match the manifest.
#[test]
fn the_routed_names_and_events_must_match_in_both_directions() {
    let mut routes = expected(false);
    routes.push(("Notification".into(), "made_up".into(), "made_up".into()));
    let mut copy = manifest()["hooks"].clone();
    copy["Notification"].as_array_mut().unwrap().push(json!({
        "matcher": "made_up",
        "hooks": [{"type": "command", "command": "gitpulse-hook notify made_up", "timeout": 5}]
    }));
    let found = problems(&copy, &routes, true);
    assert_eq!(found, vec!["a route reports made_up, which EVENTS lacks"]);

    let mut copy = manifest()["hooks"].clone();
    let at = notify_positions(&copy)
        .into_iter()
        .find(|(hook, _, _)| hook == "SessionEnd")
        .unwrap();
    delete_entry(&mut copy, &at);
    let routes: Vec<_> = expected(false)
        .into_iter()
        .filter(|(hook, _, _)| hook != "SessionEnd")
        .collect();
    let found = problems(&copy, &routes, true);
    assert_eq!(
        found,
        vec!["EVENTS has session_ended, which nothing routes"]
    );
}
