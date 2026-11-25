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
    // A platform that cannot read a process's start time (Windows has no
    // process scan) cannot tell a live writer from a reused pid, so the reader
    // refuses every snapshot there rather than trusting one it cannot check.
    let Some(my_start) = crate::sysload::process_start_us(me) else {
        let unverifiable = HubStateSnapshot {
            version: STATE_SNAPSHOT_VERSION,
            writer_pid: me,
            writer_start_us: 1,
            updated_ms: 1234,
            sessions: Vec::new(),
        };
        write_state_snapshot_atomic(&path, &unverifiable).unwrap();
        match read_state_snapshot_at(Some(&path)) {
            HubStateRead::Unavailable(reason) => {
                assert!(reason.contains("is no longer running"), "{reason}")
            }
            HubStateRead::Ok(_) => panic!("a writer that cannot be identified must be refused"),
        }
        return;
    };
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

    // Where the writer's start time cannot be read, the reader refuses every
    // snapshot, so the file is read as the worker wrote it.
    let reader_can_identify_the_writer =
        crate::sysload::process_start_us(std::process::id()).is_some();
    let saw_done = crate::test_support::wait_until(Duration::from_secs(2), || {
        let snap = if reader_can_identify_the_writer {
            match read_state_snapshot_at(Some(&path)) {
                HubStateRead::Ok(snap) => Some(snap),
                HubStateRead::Unavailable(_) => None,
            }
        } else {
            std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<HubStateSnapshot>(&bytes).ok())
        };
        snap.is_some_and(|snap| {
            snap.sessions
                .iter()
                .any(|s| s.key == "term-live" && s.state == "done")
        })
    });
    assert!(
        saw_done,
        "debounced snapshot update must publish done state"
    );

    drop(tx);
    handle.join().unwrap();
    assert!(!path.exists(), "clean worker shutdown removes snapshot");
}
