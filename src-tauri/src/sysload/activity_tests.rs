use super::*;
use crate::sysload::AgentSession;
use std::cell::Cell;

const IDLE_AFTER: u64 = 900_000;
const UUID: &str = "7b1f5c2e-1111-4222-8333-944455556666";

fn inputs() -> Inputs {
    Inputs {
        waiting: Waiting::Since(2 * IDLE_AFTER),
        cpu_rate: Reading::Value(1_000_000),
        pty_quiet_ms: Some(Reading::Value(2 * IDLE_AFTER)),
        holds_lease: false,
    }
}

fn is_idle(a: &SessionActivity) -> bool {
    matches!(a, SessionActivity::Idle { .. })
}

#[test]
fn a_session_that_waits_and_is_quiet_everywhere_is_idle_since_the_shortest_of_them() {
    let mut i = inputs();
    i.pty_quiet_ms = Some(Reading::Value(IDLE_AFTER + 5_000));
    let SessionActivity::Idle { since_ms, .. } = classify(&i, IDLE_AFTER) else {
        panic!("idle")
    };
    assert_eq!(since_ms, IDLE_AFTER + 5_000);
    // A session GitPulse holds no terminal for needs no terminal signal.
    i.pty_quiet_ms = None;
    assert!(is_idle(&classify(&i, IDLE_AFTER)));
}

#[test]
fn waiting_but_cpu_unreadable_is_unknown_not_idle() {
    for why in [Why::NotYet, Why::Unreadable, Why::Unsupported, Why::Failed] {
        let mut i = inputs();
        i.cpu_rate = Reading::Unavailable(why);
        let out = classify(&i, IDLE_AFTER);
        let SessionActivity::Unknown { why: said } = &out else {
            panic!("an unreadable CPU is not idle and not busy: {out:?}")
        };
        assert!(said.contains("CPU"), "{said}");
    }
    // The first tick has no rate yet: that is the same answer, not "quiet".
    let mut first = inputs();
    first.cpu_rate = Reading::Unavailable(Why::NotYet);
    assert!(matches!(
        classify(&first, IDLE_AFTER),
        SessionActivity::Unknown { .. }
    ));
}

#[test]
fn pty_quiet_alone_is_not_idle() {
    // The agent never reported waiting (it may be thinking silently), however
    // long its terminal has been quiet and however still its processes are.
    let mut i = inputs();
    i.waiting = Waiting::Unknown("it has made no waiting report".into());
    i.pty_quiet_ms = Some(Reading::Value(10 * IDLE_AFTER));
    assert!(matches!(
        classify(&i, IDLE_AFTER),
        SessionActivity::Unknown { .. }
    ));
    // And a terminal that printed recently overrides a waiting report.
    let mut printing = inputs();
    printing.pty_quiet_ms = Some(Reading::Value(3_000));
    let SessionActivity::Busy { why } = classify(&printing, IDLE_AFTER) else {
        panic!("busy")
    };
    assert!(why.contains("3 s ago"), "{why}");
    // A terminal GitPulse holds but could not read is unknown, not quiet.
    let mut unread = inputs();
    unread.pty_quiet_ms = Some(Reading::Unavailable(Why::Unreadable));
    assert!(matches!(
        classify(&unread, IDLE_AFTER),
        SessionActivity::Unknown { .. }
    ));
}

#[test]
fn a_lease_holder_is_never_idle() {
    let mut i = inputs();
    i.holds_lease = true;
    let SessionActivity::Busy { why } = classify(&i, IDLE_AFTER) else {
        panic!("a build holds it")
    };
    assert!(why.contains("build slot"), "{why}");
    // Even with every other signal unknown.
    i.waiting = Waiting::Unknown("x".into());
    i.cpu_rate = Reading::Unavailable(Why::Unreadable);
    assert!(matches!(
        classify(&i, IDLE_AFTER),
        SessionActivity::Busy { .. }
    ));
}

#[test]
fn a_busy_tree_or_a_short_wait_is_not_idle() {
    let mut hot = inputs();
    hot.cpu_rate = Reading::Value(CPU_IDLE_MAX);
    assert!(matches!(
        classify(&hot, IDLE_AFTER),
        SessionActivity::Busy { .. }
    ));
    let mut just_under = inputs();
    just_under.cpu_rate = Reading::Value(CPU_IDLE_MAX - 1);
    assert!(is_idle(&classify(&just_under, IDLE_AFTER)));
    let mut short = inputs();
    short.waiting = Waiting::Since(IDLE_AFTER - 1);
    assert!(matches!(
        classify(&short, IDLE_AFTER),
        SessionActivity::Busy { .. }
    ));
    short.waiting = Waiting::Since(IDLE_AFTER);
    assert!(is_idle(&classify(&short, IDLE_AFTER)));
}

#[test]
fn waiting_comes_only_from_a_report_that_asks_or_finishes_and_only_for_agents_that_report() {
    assert_eq!(
        waiting_from_standing(AgentKind::Claude, Some("idle_prompt"), 5),
        Waiting::Since(5)
    );
    assert_eq!(
        waiting_from_standing(AgentKind::Claude, Some("agent_completed"), 7),
        Waiting::Since(7)
    );
    for event in [
        Some("error"),
        Some("prompt_submitted"),
        Some("nonsense"),
        None,
    ] {
        assert!(
            matches!(
                waiting_from_standing(AgentKind::Claude, event, 9),
                Waiting::Unknown(_)
            ),
            "{event:?}"
        );
    }
    // Codex has no hooks that report a waiting state.
    let Waiting::Unknown(why) = waiting_from_standing(AgentKind::Codex, Some("idle_prompt"), 9)
    else {
        panic!("codex is unknown")
    };
    assert!(why.contains("Codex") || why.contains("codex"), "{why}");
}

fn session(kind: AgentKind, terminal: Option<&str>) -> AgentSession {
    AgentSession {
        kind,
        pid: 4_000,
        start_us: 7,
        cwd: Some("/work/repo".into()),
        tree: terminal.map(|t| format!("session:{t}")),
        run_id: None,
        parent_pid: None,
        cpu_rate: Reading::Value(0),
        footprint: Reading::Value(3 << 30),
        members: 4,
        unreadable_members: 0,
        ide_hosted: false,
        terminal_session_id: terminal.map(str::to_owned),
    }
}

fn waiting_facts() -> Facts {
    Facts {
        standing_event: Some("idle_prompt".into()),
        standing_age_ms: 2 * IDLE_AFTER,
        pty_quiet_ms: Some(Some(2 * IDLE_AFTER)),
        conversation: Some(("claude".into(), UUID.into())),
        holds_lease: false,
        launcher: Some("claude".into()),
        lease_unreadable: false,
    }
}

#[test]
fn park_refuses_codex_and_observed_sessions() {
    // A launched Claude session with a known conversation can be parked.
    assert_eq!(
        park_blocker(
            AgentKind::Claude,
            Some("term-1"),
            Some(("claude", UUID)),
            Some("/w")
        ),
        None
    );
    // An observed session: GitPulse did not launch it.
    let observed =
        park_blocker(AgentKind::Claude, None, Some(("claude", UUID)), Some("/w")).unwrap();
    assert!(observed.contains("did not launch"), "{observed}");
    // Codex cannot be resumed from GitPulse, launched or not.
    let codex = park_blocker(
        AgentKind::Codex,
        Some("term-1"),
        Some(("codex", UUID)),
        Some("/w"),
    )
    .unwrap();
    assert!(codex.contains("cannot be resumed"), "{codex}");
    // No conversation id, a malformed one, another agent's: not resumable.
    assert!(
        park_blocker(AgentKind::Claude, Some("term-1"), None, Some("/w"))
            .unwrap()
            .contains("conversation id")
    );
    assert!(park_blocker(
        AgentKind::Claude,
        Some("term-1"),
        Some(("claude", "not-a-uuid")),
        Some("/w")
    )
    .unwrap()
    .contains("could not be resumed"));
    assert!(park_blocker(
        AgentKind::Claude,
        Some("term-1"),
        Some(("agy", UUID)),
        Some("/w")
    )
    .unwrap()
    .contains("another agent"));
}

#[test]
fn the_report_calls_a_launched_waiting_session_idle_and_names_why_the_others_are_not() {
    let sessions = vec![
        session(AgentKind::Claude, Some("term-1")),
        session(AgentKind::Claude, None),
        session(AgentKind::Codex, Some("term-2")),
    ];
    let report = build(
        &sessions,
        |s| match s.terminal_session_id.as_deref() {
            Some("term-1") => waiting_facts(),
            _ => Facts::default(),
        },
        900,
        3_600,
    );
    assert_eq!((report.idle, report.unknown), (1, 2));
    let idle = &report.sessions[0];
    assert_eq!(idle.terminal_session.as_deref(), Some("term-1"));
    assert!(is_idle(&idle.activity));
    assert_eq!(idle.park_blocked, None);
    let observed = report
        .sessions
        .iter()
        .find(|s| s.terminal_session.is_none())
        .unwrap();
    assert!(matches!(observed.activity, SessionActivity::Unknown { .. }));
    assert!(observed.park_blocked.is_some());
    let codex = report
        .sessions
        .iter()
        .find(|s| s.kind == AgentKind::Codex)
        .unwrap();
    assert!(matches!(codex.activity, SessionActivity::Unknown { .. }));
}

fn idle_row() -> IdleSession {
    build(
        &[session(AgentKind::Claude, Some("term-1"))],
        |_| waiting_facts(),
        900,
        3_600,
    )
    .sessions
    .remove(0)
}

#[test]
fn a_park_asks_natively_checks_again_and_closes_nothing_without_a_yes() {
    let asked = Cell::new(0);
    let acted = Cell::new(0);
    // Declined: nothing happens.
    let declined = park(
        || Ok(idle_row()),
        &|title, body| {
            asked.set(asked.get() + 1);
            assert!(title.contains("Park"), "{title}");
            assert!(
                body.contains("/work/repo") && body.contains("resumed"),
                "{body}"
            );
            false
        },
        |_| {
            acted.set(acted.get() + 1);
            Ok("parked".into())
        },
    );
    assert!(declined.unwrap_err().contains("not confirmed"));
    assert_eq!((asked.get(), acted.get()), (1, 0));
    // Confirmed: it acts, once.
    let done = park(
        || Ok(idle_row()),
        &|_, _| true,
        |_| {
            acted.set(acted.get() + 1);
            Ok("parked".into())
        },
    );
    assert_eq!(done.unwrap(), "parked");
    assert_eq!(acted.get(), 1);
}

#[test]
fn a_session_that_stopped_being_idle_while_the_dialog_was_open_is_not_parked() {
    let reads = Cell::new(0);
    let acted = Cell::new(false);
    let out = park(
        || {
            reads.set(reads.get() + 1);
            let mut row = idle_row();
            if reads.get() > 1 {
                row.activity = SessionActivity::Busy {
                    why: "its terminal printed 1 s ago".into(),
                };
            }
            Ok(row)
        },
        &|_, _| true,
        |_| {
            acted.set(true);
            Ok(String::new())
        },
    );
    assert!(out.unwrap_err().contains("not idle"));
    assert!(!acted.get());
    // Refused before the dialog for a session that is unknown, busy or blocked.
    let asked = Cell::new(false);
    for row in [
        IdleSession {
            activity: SessionActivity::Unknown { why: "x".into() },
            ..idle_row()
        },
        IdleSession {
            activity: SessionActivity::Busy { why: "x".into() },
            ..idle_row()
        },
        IdleSession {
            park_blocked: Some("GitPulse did not launch it".into()),
            ..idle_row()
        },
    ] {
        let out = park(
            || Ok(row.clone()),
            &|_, _| {
                asked.set(true);
                true
            },
            |_| Ok(String::new()),
        );
        assert!(out.is_err());
    }
    assert!(
        !asked.get(),
        "nothing is asked about a session that cannot be parked"
    );
}

#[test]
fn an_unreadable_build_slot_state_keeps_a_waiting_session_from_being_called_idle() {
    let sessions = [session(AgentKind::Claude, Some("term-1"))];
    let read = build(&sessions, |_| waiting_facts(), 900, 3_600);
    assert_eq!(read.idle, 1);
    let unread = build(
        &sessions,
        |_| Facts {
            lease_unreadable: true,
            ..waiting_facts()
        },
        900,
        3_600,
    );
    assert_eq!((unread.idle, unread.unknown), (0, 1));
    let SessionActivity::Unknown { why } = &unread.sessions[0].activity else {
        panic!("unknown")
    };
    assert!(why.contains("build slot"), "{why}");
}

fn two_in_one_terminal() -> Vec<AgentSession> {
    let mut second = session(AgentKind::Claude, Some("term-1"));
    second.pid = 4_001;
    vec![session(AgentKind::Claude, Some("term-1")), second]
}

#[test]
fn a_shell_tab_with_an_agent_typed_into_it_is_not_parked() {
    // Every PTY carries a GitPulse session id; only a tab launched AS the agent
    // may be closed to park it.
    let sessions = [session(AgentKind::Claude, Some("term-1"))];
    for (launcher, why) in [
        (None, "not launched as this agent"),
        (Some("codex"), "launched as codex, not as claude"),
    ] {
        let read = build(
            &sessions,
            |_| Facts {
                launcher: launcher.map(str::to_owned),
                ..waiting_facts()
            },
            900,
            3_600,
        );
        let blocked = read.sessions[0].park_blocked.as_deref().expect("blocked");
        assert!(blocked.contains(why), "{blocked}");
        // Still idle: the refusal is about closing the tab, not about the agent.
        assert!(is_idle(&read.sessions[0].activity));
    }
    // And launched as that agent, it can be.
    let ok = build(&sessions, |_| waiting_facts(), 900, 3_600);
    assert_eq!(ok.sessions[0].park_blocked, None);
}

#[test]
fn a_terminal_that_hosts_two_agents_parks_neither() {
    let read = build(&two_in_one_terminal(), |_| waiting_facts(), 900, 3_600);
    assert_eq!(read.sessions.len(), 2);
    for row in &read.sessions {
        let blocked = row.park_blocked.as_deref().expect("blocked");
        assert!(
            blocked.contains("2 agents run in that terminal"),
            "{blocked}"
        );
    }
}

#[test]
fn park_enforces_the_park_after_time_itself_not_only_what_health_offers() {
    let acted = Cell::new(false);
    // Idle for 1.8 million ms (`waiting_facts`), against a one hour minimum.
    let early = park_when_idle_for(
        || Ok(idle_row()),
        &|_, _| true,
        |_| {
            acted.set(true);
            Ok(String::new())
        },
        3_600_000,
    );
    assert!(early
        .unwrap_err()
        .contains("under the 3600 s set for parking"));
    assert!(!acted.get());
    let late = park_when_idle_for(
        || Ok(idle_row()),
        &|_, _| true,
        |_| Ok("parked".into()),
        1_800_000,
    );
    assert_eq!(late.unwrap(), "parked");
}
