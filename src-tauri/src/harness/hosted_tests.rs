use super::*;
use std::os::unix::fs::PermissionsExt;

fn dir() -> tempfile::TempDir {
    let base = std::env::temp_dir().canonicalize().unwrap();
    tempfile::Builder::new()
        .prefix("ph")
        .tempdir_in(base)
        .unwrap()
}

fn status(owner: &str, pid: u32, protocol: i64) -> ServeStatus {
    ServeStatus {
        protocol,
        owner_id: owner.into(),
        process_id: pid,
        controller: true,
        jobs: Vec::new(),
    }
}

#[test]
fn an_owner_is_parsed_in_one_place_and_only_its_exact_shape_counts() {
    assert_eq!(
        manvi_owner("manvi-42-1700000000000000000-ab12cd"),
        Some((42, 1_700_000_000_000_000_000))
    );
    for bad in [
        "",
        "manvi-",
        "manvi-0-1-ab",
        "manvi-x-1-ab",
        "manvi-42-x-ab",
        "manvi-42-1-",
        "manvi-42-1-zz",
        "manvi-42-1-ab-extra",
        "native-42-1-3",
    ] {
        assert_eq!(manvi_owner(bad), None, "{bad:?}");
    }
}

#[test]
fn the_server_is_judged_by_how_it_was_started_and_whether_it_holds_runs() {
    let paths = HostPaths::under(Path::new("/x"));
    let record = |selection: &str, restart: bool| Record {
        restart,
        ..Record::new(
            &paths,
            &status("manvi-1-1-ab", 1, 1),
            selection,
            Path::new("/p"),
        )
    };
    assert_eq!(
        judge_server(&record("default", false), "default", 5),
        Reuse::Yes
    );
    // Out of date and idle: replaced. Out of date and busy: kept for its runs.
    assert_eq!(
        judge_server(&record("default", false), "claude|m", 0),
        Reuse::Replace
    );
    assert_eq!(
        judge_server(&record("default", false), "claude|m", 3),
        Reuse::KeepForRuns(3)
    );
    // A key that changed is the same as a model that did.
    assert_eq!(
        judge_server(&record("default", true), "default", 0),
        Reuse::Replace
    );
    assert_eq!(
        judge_server(&record("default", true), "default", 1),
        Reuse::KeepForRuns(1)
    );
}

#[test]
fn a_fingerprint_tells_selections_apart_and_carries_no_credential() {
    assert_eq!(fingerprint(None), "default");
    let local = ModelSelection::Local {
        base_url: "http://127.0.0.1:1234".into(),
        model: "m".into(),
    };
    assert_eq!(fingerprint(Some(&local)), "local|http://127.0.0.1:1234|m");
    assert_eq!(
        fingerprint(Some(&ModelSelection::Claude { model: "c".into() })),
        "claude|c"
    );
}

#[test]
fn a_status_must_name_a_manvi_whose_process_is_the_one_answering() {
    let ok = status("manvi-42-9-ab", 42, 1);
    assert_eq!(ok.verify_identity(), Ok((42, 9)));
    assert!(status("manvi-42-9-ab", 42, 2)
        .verify_identity()
        .unwrap_err()
        .contains("protocol 2"));
    assert!(status("someone", 42, 1)
        .verify_identity()
        .unwrap_err()
        .contains("not a Manvi owner id"));
    assert!(status("manvi-42-9-ab", 43, 1)
        .verify_identity()
        .unwrap_err()
        .contains("reports process 43"));
}

#[test]
fn a_job_list_at_the_cap_may_have_been_cut() {
    let mut status = status("manvi-1-1-ab", 1, 1);
    status.jobs = (0..MAX_LISTED_JOBS - 1)
        .map(|n| ServeJob {
            id: n.to_string(),
            phase: "running".into(),
        })
        .collect();
    assert!(status.jobs_complete());
    status.jobs.push(ServeJob {
        id: "last".into(),
        phase: "running".into(),
    });
    assert!(!status.jobs_complete());
}

#[test]
fn the_record_round_trips_privately_and_refuses_what_it_cannot_read() {
    let tmp = dir();
    let paths = HostPaths::under(&tmp.path().join("h"));
    paths.prepare().unwrap();
    assert_eq!(read_record(&paths), Ok(None));

    let record = Record::new(
        &paths,
        &status("manvi-7-1-ab", 7, 1),
        "default",
        Path::new("/p"),
    );
    write_record(&paths, &record).unwrap();
    assert_eq!(read_record(&paths).unwrap(), Some(record.clone()));
    let mode = std::fs::metadata(&paths.record)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "the record is private to the user");
    assert!(is_private(&paths.dir));

    mark_restart(&paths).unwrap();
    assert!(read_record(&paths).unwrap().unwrap().restart);

    for (text, why) in [
        ("{\"version\":1}", "unreadable"),
        ("not json", "unreadable"),
        ("{\"version\":9,\"socket\":\"s\",\"owner_id\":\"o\",\"process_id\":1,\"protocol\":1,\"selection\":\"d\",\"profile\":\"p\"}", "version 9"),
        ("{\"version\":1,\"socket\":\"s\",\"owner_id\":\"o\",\"process_id\":1,\"protocol\":1,\"selection\":\"d\",\"profile\":\"p\",\"extra\":1}", "unreadable"),
    ] {
        std::fs::write(&paths.record, text).unwrap();
        let error = read_record(&paths).unwrap_err();
        assert!(error.contains(why), "{text}: {error}");
    }
    std::fs::write(&paths.record, vec![b' '; (MAX_RECORD_BYTES + 1) as usize]).unwrap();
    assert!(read_record(&paths).unwrap_err().contains("too large"));

    // A link planted where the record goes is never followed.
    std::fs::remove_file(&paths.record).unwrap();
    std::os::unix::fs::symlink("/etc/hosts", &paths.record).unwrap();
    assert!(read_record(&paths).is_err());
}

#[test]
fn marking_a_restart_with_no_record_marks_nothing() {
    let tmp = dir();
    let paths = HostPaths::under(&tmp.path().join("h"));
    paths.prepare().unwrap();
    mark_restart(&paths).unwrap();
    assert_eq!(read_record(&paths), Ok(None));
}

/// "Its log ends" must be the end: a server that logged more than the read
/// window said why it failed on its last lines, not its first.
#[test]
fn the_log_tail_is_the_end_of_a_log_longer_than_the_window() {
    let tmp = dir();
    let log = tmp.path().join("serve.log");
    let mut text = String::new();
    for n in 0..400 {
        text.push_str(&format!("startup line {n:04} {}\n", "x".repeat(20)));
    }
    text.push_str("fatal: cannot open the profile\nfatal: giving up\nexit status 1\n");
    assert!(text.len() > 2 * 4096, "the log is well over the window");
    std::fs::write(&log, &text).unwrap();
    assert_eq!(
        log_tail(&log),
        "fatal: cannot open the profile / fatal: giving up / exit status 1"
    );

    // Short logs, a missing log and an empty one keep their old answers.
    std::fs::write(&log, "one\ntwo\n").unwrap();
    assert_eq!(log_tail(&log), "one / two");
    std::fs::write(&log, "").unwrap();
    assert_eq!(log_tail(&log), "");
    assert_eq!(log_tail(&tmp.path().join("missing.log")), "");

    // A window that opens inside a line never reports the fragment.
    let long_line = "y".repeat(10_000);
    std::fs::write(&log, format!("{long_line}\nlast words\n")).unwrap();
    assert_eq!(log_tail(&log), "last words");
}

#[test]
fn a_record_is_about_the_socket_that_was_dialled() {
    let paths = HostPaths::under(Path::new("/x"));
    let record = Record::new(
        &paths,
        &status("manvi-1-1-ab", 1, 1),
        "default",
        Path::new("/p"),
    );
    assert_eq!(record.verify_socket(&paths), Ok(true));
    let moved = HostPaths::under(Path::new("/y"));
    let why = record.verify_socket(&moved).unwrap_err();
    assert!(
        why.contains("/x/serve.sock") && why.contains("/y/serve.sock"),
        "{why}"
    );
    // A record from before the path was kept says nothing either way.
    let unrecorded = Record {
        socket: String::new(),
        ..record
    };
    assert_eq!(unrecorded.verify_socket(&moved), Ok(false));
}

#[test]
fn a_socket_path_too_long_for_a_unix_socket_is_refused_before_anything_starts() {
    let tmp = dir();
    let long = tmp
        .path()
        .join("x".repeat(crate::local_socket::max_path_bytes()));
    let error = HostPaths::under(&long).prepare().unwrap_err();
    assert!(error.contains("this system allows"), "{error}");
    assert!(
        !long.exists(),
        "nothing is created for a path that cannot be bound"
    );
}

fn fake_manvi(tmp: &Path, name: &str, body: &str) -> String {
    let path = tmp.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path.display().to_string()
}

#[test]
fn support_is_what_the_installed_manvi_says_about_serving_and_is_remembered_per_binary() {
    let tmp = dir();
    let counter = tmp.path().join("asked");
    let supported = fake_manvi(
        tmp.path(),
        "new",
        &format!(
            "echo ask >> {}\necho 'manvi serve --listen /abs/serve.sock'",
            counter.display()
        ),
    );
    assert_eq!(probe_support(&supported), Support::Supported);
    assert_eq!(probe_support(&supported), Support::Supported);
    assert_eq!(
        std::fs::read_to_string(&counter).unwrap().lines().count(),
        1,
        "the second answer comes from memory"
    );

    let old = fake_manvi(tmp.path(), "old", "echo 'manvi serve [--posture host]'");
    match probe_support(&old) {
        Support::Unsupported(why) => assert!(why.contains("cannot serve on a socket"), "{why}"),
        other => panic!("{other:?}"),
    }

    // A binary that cannot be asked is unknown, not unsupported, and is asked again.
    let broken = fake_manvi(tmp.path(), "broken", "exit 3");
    assert!(matches!(probe_support(&broken), Support::Unknown(_)));
    assert!(matches!(
        probe_support(&tmp.path().join("missing").display().to_string()),
        Support::Unknown(_)
    ));
}

#[test]
fn what_is_said_about_hosting_never_claims_more_than_was_observed() {
    let ends = "Managed agent runs end when GitPulse quits";
    let off = describe(false, &Observed::Untried, None);
    assert_eq!((off.requested, off.hosted), (false, false));
    assert_eq!(off.detail, format!("{ends}."));

    let on = describe(true, &Observed::Hosted, None);
    assert!(on.requested && on.hosted);
    assert!(on.detail.contains("kept running"), "{}", on.detail);

    // Detached is not hosted: nothing holds the runs right now.
    assert!(!describe(true, &Observed::Detached, Some(&Support::Supported)).hosted);

    let fell = describe(
        true,
        &Observed::FellBack("it broke".into()),
        Some(&Support::Supported),
    );
    assert!(!fell.hosted);
    assert_eq!(fell.detail, format!("{ends}: it broke."));
    let blocked = describe(true, &Observed::Blocked("busy".into()), None);
    assert!(!blocked.hosted && blocked.detail.contains("busy"));

    let unsupported = describe(
        true,
        &Observed::Untried,
        Some(&Support::Unsupported("old".into())),
    );
    assert!(!unsupported.hosted);
    assert_eq!(unsupported.detail, format!("{ends}: old."));
    let unknown = describe(
        true,
        &Observed::Untried,
        Some(&Support::Unknown("no answer".into())),
    );
    assert!(
        unknown.detail.contains("could not be checked: no answer"),
        "{}",
        unknown.detail
    );
    let pending = describe(true, &Observed::Untried, Some(&Support::Supported));
    assert!(!pending.hosted && pending.detail.contains("once the agent runtime starts"));
    let unasked = describe(true, &Observed::Untried, None);
    assert!(
        unasked.detail.contains("has not been checked"),
        "{}",
        unasked.detail
    );
}

#[test]
fn an_exit_nobody_was_asked_about_leaves_the_runs() {
    let _serial = crate::harness::sidecar::test_serial();
    reset_exit_choice();
    assert_eq!(exit_choice(), ExitChoice::Leave);
    set_exit_choice(ExitChoice::Stop);
    assert_eq!(exit_choice(), ExitChoice::Stop);
    reset_exit_choice();
}

/// What keeps another user off the socket on disk: the directory that holds it
/// is made private, whatever mode it had, and is refused when it is reached
/// through a symlink. (The dial's owner and peer-uid refusals are not
/// exercised across two real users here: one test user cannot be two.)
#[test]
fn the_directory_holding_the_socket_is_made_private_and_never_a_symlink() {
    let tmp = dir();
    let open = tmp.path().join("open");
    std::fs::create_dir(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
    let paths = HostPaths::under(&open);
    assert!(!is_private(&open));
    paths.prepare().unwrap();
    assert!(is_private(&paths.dir));
    assert!(is_private(&paths.state));

    let real = tmp.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(HostPaths::under(&link).prepare().is_err());
}

/// Manvi refuses a socket path over 100 bytes, which is shorter than what the
/// system allows (103 on macOS): a path in between must be refused here, with
/// the reason, and not handed to a server that will refuse it.
#[test]
fn a_socket_path_longer_than_manvi_accepts_is_refused_before_it_is_started() {
    let dir = std::path::PathBuf::from(format!("/{}", "a".repeat(89)));
    let paths = HostPaths::under(&dir);
    assert_eq!(paths.socket.as_os_str().len(), 101);
    let error = paths.prepare().unwrap_err();
    assert!(
        error.contains("101 bytes") && error.contains("allows 100"),
        "{error}"
    );
    assert!(
        !dir.exists(),
        "nothing was created for a path that cannot be used"
    );
}
