use super::*;
use crate::alerts::intake::LowGate;
use std::sync::mpsc;

fn hook_notice(
    key: &str,
    event: Option<&'static str>,
    provider_session_id: Option<&str>,
    place: Option<&str>,
) -> Notice {
    Notice {
        key: key.to_owned(),
        origin: Origin::Hook,
        label: "claude".into(),
        place: place.map(str::to_owned),
        event,
        detail: None,
        is_agent: true,
        channel: "hook",
        subject: Some("deadbeef".into()),
        subagent: false,
        agent_id: None,
        fully_idle: None,
        provider_session_id: provider_session_id.map(str::to_owned),
    }
}

#[test]
fn hub_state_transitions_preserve_age_across_restatements_and_clear_on_end() {
    let mut tracked: HashMap<String, Tracked> = HashMap::new();
    let counters = Counters::default();
    let t0 = Instant::now();

    let _ = admit(
        hook_notice(
            "term-1",
            Some("permission_request"),
            Some("sid-1"),
            Some("/repo"),
        ),
        t0,
        &mut tracked,
        &counters,
    );
    let first = snapshot_of(&tracked, 42, 100, 1000);
    assert_eq!(first.sessions.len(), 1);
    assert_eq!(first.sessions[0].state, "blocked");
    assert_eq!(
        first.sessions[0].event.as_deref(),
        Some("permission_request")
    );
    assert_eq!(
        first.sessions[0].provider_session_id.as_deref(),
        Some("sid-1")
    );
    assert_eq!(first.sessions[0].place.as_deref(), Some("/repo"));
    let blocked_since = first.sessions[0].changed_ms;

    // permission_prompt restates the same blocked state without resetting changed_ms.
    let _ = admit(
        hook_notice("term-1", Some("permission_prompt"), None, None),
        t0 + Duration::from_secs(6),
        &mut tracked,
        &counters,
    );
    let restated = snapshot_of(&tracked, 42, 100, 2000);
    assert_eq!(restated.sessions[0].state, "blocked");
    assert_eq!(restated.sessions[0].changed_ms, blocked_since);
    assert_eq!(
        restated.sessions[0].provider_session_id.as_deref(),
        Some("sid-1")
    );

    // Matching tool_finished transitions to working.
    let _ = admit(
        hook_notice("term-1", Some("tool_finished"), None, None),
        t0 + Duration::from_secs(7),
        &mut tracked,
        &counters,
    );
    assert_eq!(
        snapshot_of(&tracked, 42, 100, 3000).sessions[0].state,
        "working"
    );

    // turn_finished -> done; idle_prompt -> idle.
    let _ = admit(
        hook_notice("term-1", Some("turn_finished"), None, None),
        t0 + Duration::from_secs(8),
        &mut tracked,
        &counters,
    );
    assert_eq!(
        snapshot_of(&tracked, 42, 100, 4000).sessions[0].state,
        "done"
    );

    let _ = admit(
        hook_notice("term-1", Some("idle_prompt"), None, None),
        t0 + Duration::from_secs(70),
        &mut tracked,
        &counters,
    );
    assert_eq!(
        snapshot_of(&tracked, 42, 100, 5000).sessions[0].state,
        "idle"
    );

    // session_ended removes the entry from hub state so stale keys do not linger.
    let _ = admit(
        hook_notice("term-1", Some("session_ended"), None, None),
        t0 + Duration::from_secs(80),
        &mut tracked,
        &counters,
    );
    assert!(snapshot_of(&tracked, 42, 100, 6000).sessions.is_empty());
}

fn terminal_signal(key: &str, channel: &'static str, detail: Option<&str>) -> Notice {
    Notice {
        key: key.to_owned(),
        origin: Origin::Terminal,
        label: "Claude Code".into(),
        place: Some("/repo".into()),
        event: None,
        detail: detail.map(str::to_owned),
        is_agent: true,
        channel,
        subject: None,
        subagent: false,
        agent_id: None,
        fully_idle: None,
        provider_session_id: None,
    }
}

#[test]
fn mapped_standing_state_separates_error_idle_and_informational_signals_from_blocked() {
    let mut tracked: HashMap<String, Tracked> = HashMap::new();
    let counters = Counters::default();
    let t0 = Instant::now();

    // Hook `error` maps to `"error"` (not `"blocked"`).
    let _ = admit_at(
        hook_notice("term-err", Some("error"), None, None),
        t0,
        1_000,
        &mut tracked,
        &counters,
    );
    // Bare BEL and informational OSC map to `"idle"` (never `"blocked"`).
    let _ = admit_at(
        terminal_signal("term-bell", "bell", None),
        t0,
        1_000,
        &mut tracked,
        &counters,
    );
    let _ = admit_at(
        terminal_signal("term-osc", "osc9", Some("Build done")),
        t0,
        1_000,
        &mut tracked,
        &counters,
    );
    // Hook `idle_prompt` maps to `"idle"`, while `agent_needs_input` maps to `"blocked"`.
    let _ = admit_at(
        hook_notice("term-idle", Some("idle_prompt"), None, None),
        t0,
        1_000,
        &mut tracked,
        &counters,
    );
    let _ = admit_at(
        hook_notice("term-ask", Some("agent_needs_input"), None, None),
        t0,
        1_000,
        &mut tracked,
        &counters,
    );

    // Post-HOOK_SHADOW bare BEL and OSC 9 must never clobber `"blocked"` hook state.
    let later = t0 + HOOK_SHADOW + Duration::from_secs(5);
    let _ = admit_at(
        terminal_signal("term-ask", "bell", None),
        later,
        20_000,
        &mut tracked,
        &counters,
    );
    let _ = admit_at(
        terminal_signal("term-ask", "osc9", Some("FYI")),
        later + Duration::from_secs(1),
        21_000,
        &mut tracked,
        &counters,
    );

    let snap = snapshot_of(&tracked, 42, 100, 22_000);
    let states: HashMap<&str, &str> = snap
        .sessions
        .iter()
        .map(|s| (s.key.as_str(), s.state.as_str()))
        .collect();
    assert_eq!(states.get("term-err"), Some(&"error"));
    assert_eq!(states.get("term-bell"), Some(&"idle"));
    assert_eq!(states.get("term-osc"), Some(&"idle"));
    assert_eq!(states.get("term-idle"), Some(&"idle"));
    assert_eq!(states.get("term-ask"), Some(&"blocked"));
}

#[test]
fn snapshot_reader_rejects_missing_dead_and_reused_writers_and_sets_0600() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join(STATE_SNAPSHOT_NAME);

    assert!(matches!(
        read_state_snapshot_at(Some(&path)),
        HubStateRead::Unavailable(_)
    ));

    let me = std::process::id();
    let my_start = crate::sysload::process_start_us(me).unwrap();
    let snap = HubStateSnapshot {
        version: STATE_SNAPSHOT_VERSION,
        writer_pid: me,
        writer_start_us: my_start,
        updated_ms: 1234,
        sessions: vec![HubSessionState {
            key: "term-1".into(),
            provider_session_id: Some("sid-1".into()),
            place: Some("/repo".into()),
            state: "blocked".into(),
            event: Some("permission_request".into()),
            changed_ms: 1000,
            active_subagents: Vec::new(),
            fully_idle: None,
        }],
    };
    write_state_snapshot_atomic(&path, &snap).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    match read_state_snapshot_at(Some(&path)) {
        HubStateRead::Ok(loaded) => assert_eq!(loaded, snap),
        HubStateRead::Unavailable(err) => panic!("expected ok snapshot, got: {err}"),
    }

    // PID reuse: same live pid, different start_us.
    let reused = HubStateSnapshot {
        writer_start_us: my_start.saturating_add(1),
        ..snap.clone()
    };
    write_state_snapshot_atomic(&path, &reused).unwrap();
    assert!(matches!(
        read_state_snapshot_at(Some(&path)),
        HubStateRead::Unavailable(_)
    ));

    // Dead PID.
    let dead = HubStateSnapshot {
        writer_pid: u32::MAX / 2,
        ..snap
    };
    write_state_snapshot_atomic(&path, &dead).unwrap();
    assert!(matches!(
        read_state_snapshot_at(Some(&path)),
        HubStateRead::Unavailable(_)
    ));
}

struct SnapshotHost {
    path: PathBuf,
}

impl Host for SnapshotHost {
    fn submit(
        &self,
        _native: &str,
        _heading: &str,
        _body: &str,
        _sound: bool,
    ) -> Result<bool, String> {
        Ok(false)
    }
    fn attended(&self, _key: &str) -> bool {
        false
    }
    fn state_snapshot_path(&self) -> Option<PathBuf> {
        Some(self.path.clone())
    }
}

#[test]
fn worker_writes_initial_snapshot_debounced_updates_and_removes_on_exit() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join(STATE_SNAPSHOT_NAME);
    let (tx, rx) = mpsc::channel();
    let bells = Arc::new(LowGate::default());
    let counters = Arc::new(Counters::default());
    let last_error = Arc::new(Mutex::new(None));
    let standing = Arc::new(Mutex::new(Vec::new()));
    let host: Arc<dyn Host> = Arc::new(SnapshotHost { path: path.clone() });

    let handle = std::thread::spawn(move || {
        run(rx, bells, host, counters, last_error, standing);
    });

    assert!(
        crate::test_support::wait_until(Duration::from_secs(2), || path.exists()),
        "initial snapshot must appear at startup"
    );

    tx.send(hook_notice(
        "term-live",
        Some("turn_finished"),
        Some("sid-live"),
        None,
    ))
    .unwrap();

    let saw_done = crate::test_support::wait_until(Duration::from_secs(2), || {
        matches!(
            read_state_snapshot_at(Some(&path)),
            HubStateRead::Ok(snap)
                if snap
                    .sessions
                    .iter()
                    .any(|s| s.key == "term-live" && s.state == "done")
        )
    });
    assert!(
        saw_done,
        "debounced snapshot update must publish done state"
    );

    drop(tx);
    handle.join().unwrap();
    assert!(!path.exists(), "clean worker shutdown removes snapshot");
}

#[test]
fn hub_session_state_projects_subagents_and_fully_idle_with_backward_compatible_serde() {
    // Legacy JSON without `active_subagents` / `fully_idle` still deserializes cleanly.
    let legacy_json = r#"{
        "key": "term-legacy",
        "state": "working",
        "changed_ms": 500
    }"#;
    let parsed: HubSessionState = serde_json::from_str(legacy_json).unwrap();
    assert!(parsed.active_subagents.is_empty());
    assert_eq!(parsed.fully_idle, None);
    let reencoded = serde_json::to_string(&parsed).unwrap();
    assert!(
        !reencoded.contains("active_subagents"),
        "empty active_subagents must skip_serializing_if empty: {reencoded}"
    );
    assert!(!reencoded.contains("fully_idle"));

    let mut tracked: HashMap<String, Tracked> = HashMap::new();
    let counters = Counters::default();
    let t0 = Instant::now();

    let mut parent = hook_notice("term-swarm", Some("prompt_submitted"), Some("sid-s"), Some("/repo"));
    parent.fully_idle = Some(false);
    let _ = admit_at(parent, t0, 1_000, &mut tracked, &counters);

    // Non-blocking subagent reports track under `active_subagents` without clobbering parent state.
    let mut sub_a = hook_notice("term-swarm", Some("tool_finished"), None, None);
    sub_a.subagent = true;
    sub_a.agent_id = Some("sub-alpha".into());
    sub_a.detail = Some("running tests".into());
    let _ = admit_at(sub_a, t0 + Duration::from_secs(1), 2_000, &mut tracked, &counters);

    let mut sub_b = hook_notice("term-swarm", Some("permission_request"), None, None);
    sub_b.subagent = true;
    sub_b.agent_id = Some("sub-beta".into());
    sub_b.detail = Some("Bash: cargo build".into());
    let _ = admit_at(sub_b, t0 + Duration::from_secs(2), 3_000, &mut tracked, &counters);

    let snap = snapshot_of(&tracked, 42, 100, 3_500);
    assert_eq!(snap.sessions.len(), 1);
    let session = &snap.sessions[0];
    assert_eq!(session.fully_idle, Some(false));
    assert_eq!(session.active_subagents.len(), 2);
    assert_eq!(session.active_subagents[0].agent_id, "sub-alpha");
    assert_eq!(session.active_subagents[0].state, "working");
    assert_eq!(session.active_subagents[1].agent_id, "sub-beta");
    assert_eq!(session.active_subagents[1].state, "blocked");
    assert_eq!(
        session.active_subagents[1].detail.as_deref(),
        Some("Bash: cargo build")
    );

    // Subagent ending removes only that subagent; parent fully_idle updates to true.
    let mut sub_a_end = hook_notice("term-swarm", Some("session_ended"), None, None);
    sub_a_end.subagent = true;
    sub_a_end.agent_id = Some("sub-alpha".into());
    let _ = admit_at(
        sub_a_end,
        t0 + Duration::from_secs(3),
        4_000,
        &mut tracked,
        &counters,
    );

    let mut parent_idle = hook_notice("term-swarm", Some("turn_finished"), None, None);
    parent_idle.fully_idle = Some(true);
    let _ = admit_at(
        parent_idle,
        t0 + Duration::from_secs(4),
        5_000,
        &mut tracked,
        &counters,
    );

    let after = snapshot_of(&tracked, 42, 100, 5_500);
    assert_eq!(after.sessions[0].fully_idle, Some(true));
    assert_eq!(after.sessions[0].active_subagents.len(), 1);
    assert_eq!(after.sessions[0].active_subagents[0].agent_id, "sub-beta");
}
