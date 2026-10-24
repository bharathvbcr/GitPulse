//! Adversarial tests for the accept-loop hardening: the connection cap, the
//! rebind/stop races, the transient backoff and the deadline.
use super::*;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

fn cpu_time() -> Duration {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `getrusage` fills the struct it is given.
    let usage = unsafe {
        libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr());
        usage.assume_init()
    };
    let tv = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
    tv(usage.ru_utime) + tv(usage.ru_stime)
}

/// Closes with a reset instead of a FIN.
fn reset(stream: UnixStream) {
    use std::os::unix::io::AsRawFd;
    let linger = libc::linger {
        l_onoff: 1,
        l_linger: 0,
    };
    // SAFETY: a valid fd and a correctly sized option value.
    unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            std::ptr::addr_of!(linger).cast(),
            std::mem::size_of::<libc::linger>() as libc::socklen_t,
        );
    }
    drop(stream);
}

/// Accept/close churn of every shape must leave no slot behind.
#[test]
fn connection_churn_of_every_shape_does_not_leak_slots() {
    let _own = own_listener();
    let dir = ShortDir::new("advchurn");
    apply_at(true, Some(dir.socket()));
    assert!(status().listening, "{:?}", status().error);
    let path = dir.socket();
    let workers: Vec<_> = (0..8)
        .map(|t| {
            let path = path.clone();
            std::thread::spawn(move || {
                for i in 0..300 {
                    let Ok(mut s) = UnixStream::connect(&path) else {
                        continue;
                    };
                    match (t + i) % 5 {
                        0 => drop(s),
                        1 => {
                            let _ = s.write_all(&REPORT[..10]);
                            drop(s);
                        }
                        2 => {
                            let _ = s.write_all(&REPORT[..10]);
                            reset(s);
                        }
                        3 => reset(s),
                        _ => {
                            let _ = s.write_all(REPORT);
                            let _ = s.shutdown(std::net::Shutdown::Write);
                            let mut reply = String::new();
                            let _ = s.read_to_string(&mut reply);
                        }
                    }
                }
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
    let drained = within(Duration::from_secs(8), || open_connections() == 0);
    let left = open_connections();
    let landed = within(Duration::from_secs(5), || {
        send(&path, REPORT).trim() == r#"{"ok":true}"#
    });
    apply_at(false, None);
    assert!(drained, "{left} slots still held after the churn");
    assert!(landed, "a real report was refused after the churn");
}

/// A fatal accept error racing `stop`: afterwards nothing is bound, no socket
/// file remains and no loop is alive.
#[test]
fn a_fatal_accept_error_racing_stop_leaves_nothing_behind() {
    let _own = own_listener();
    let _clear = ClearInjected;
    let dir = ShortDir::new("advrace");
    for round in 0..80u64 {
        apply_at(true, Some(dir.socket()));
        inject_accept_errors(&vec![libc::EBADF; 1 + (round % 3) as usize]);
        let path = dir.socket();
        let poker = std::thread::spawn(move || {
            for _ in 0..6 {
                let _ = UnixStream::connect(&path);
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        std::thread::sleep(Duration::from_millis((round * 7) % 45));
        // Alternate: plain stop, and stop then an immediate restart.
        apply_at(false, None);
        if round % 2 == 1 {
            apply_at(true, Some(dir.socket()));
            apply_at(false, None);
        }
        poker.join().unwrap();
        ClearInjected.drop_now();
        assert!(
            within(Duration::from_secs(5), || live_loops() == 0),
            "round {round}: {} loops alive after stop",
            live_loops()
        );
        assert!(!status().listening, "round {round}: still listening");
        assert!(
            !dir.socket().exists(),
            "round {round}: a stopped bridge left its socket"
        );
    }
}

impl ClearInjected {
    fn drop_now(&self) {
        while injected_remaining() > 0 {
            let _ = injected_accept_error();
        }
    }
}

/// After `stop`, the status must not show an error from the dead loop.
#[test]
fn a_backoff_that_begins_after_stop_does_not_leave_an_error_behind() {
    let _own = own_listener();
    let _clear = ClearInjected;
    let dir = ShortDir::new("advstale");
    apply_at(true, Some(dir.socket()));
    assert!(within(Duration::from_secs(5), || live_loops() == 1));
    HOLD_AFTER_WAIT_MS.store(400, Ordering::SeqCst);
    inject_accept_errors(&[libc::EMFILE]);
    let _poke = UnixStream::connect(dir.socket()).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    apply_at(false, None);
    let ended = within(Duration::from_secs(5), || live_loops() == 0);
    HOLD_AFTER_WAIT_MS.store(0, Ordering::SeqCst);
    assert!(ended);
    assert_eq!(injected_remaining(), 0, "the loop never reached accept");
    assert_eq!(
        status().error,
        None,
        "a stopped bridge shows a stale accept-loop error"
    );
}

/// The backoff's own `set_error(None)` must not erase an error it did not set.
#[test]
fn a_backoff_does_not_wipe_an_unrelated_error() {
    let _own = own_listener();
    let _clear = ClearInjected;
    let dir = ShortDir::new("advwipe");
    apply_at(true, Some(dir.socket()));
    assert!(within(Duration::from_secs(5), || live_loops() == 1));
    set_error(Some("unrelated: needs the user".into()));
    inject_accept_errors(&[libc::EMFILE]);
    poke_until(&dir.socket(), || injected_remaining() == 0);
    std::thread::sleep(Duration::from_millis(300));
    let error = status().error;
    apply_at(false, None);
    assert_eq!(
        error.as_deref(),
        Some("unrelated: needs the user"),
        "the backoff replaced or erased another error"
    );
}

/// Descriptor exhaustion with a connection still pending: the loop backs off
/// instead of spinning.
#[test]
fn an_emfile_storm_with_a_pending_connection_does_not_spin() {
    let _own = own_listener();
    let _clear = ClearInjected;
    let dir = ShortDir::new("advemfile");
    apply_at(true, Some(dir.socket()));
    assert!(within(Duration::from_secs(5), || live_loops() == 1));
    inject_accept_errors(&[libc::EMFILE; 4000]);
    let _pending = UnixStream::connect(dir.socket()).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let before_cpu = cpu_time();
    let before_left = injected_remaining();
    let started = Instant::now();
    std::thread::sleep(Duration::from_secs(3));
    let cpu = cpu_time() - before_cpu;
    let calls = before_left - injected_remaining();
    let wall = started.elapsed();
    apply_at(false, None);
    eprintln!("EMFILE storm: {calls} accept attempts, {cpu:?} cpu in {wall:?}");
    assert!(calls < 40, "{calls} accept attempts in {wall:?}: the loop is spinning");
    assert!(cpu < Duration::from_millis(150), "{cpu:?} cpu in {wall:?}");
}

/// A full report with no half-close is dropped at the deadline and the peer
/// reaches EOF before the hook's own budget runs out, with no reply.
#[test]
fn a_report_without_half_close_is_dropped_and_the_client_sees_eof_in_time() {
    let _own = own_listener();
    let dir = ShortDir::new("advnoclose");
    apply_at(true, Some(dir.socket()));
    assert!(status().listening, "{:?}", status().error);
    let mut s = UnixStream::connect(dir.socket()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(REPORT).unwrap();
    let started = Instant::now();
    let mut reply = Vec::new();
    let result = s.read_to_end(&mut reply);
    let waited = started.elapsed();
    apply_at(false, None);
    assert!(result.is_ok(), "{result:?}");
    eprintln!("no half-close: EOF after {waited:?}, reply {reply:?}");
    assert!(
        waited < crate::hooks::NOTIFY_BUDGET,
        "EOF after {waited:?}, past the hook's budget"
    );
    assert!(reply.is_empty() || reply == b"{\"ok\":false}\n", "{reply:?}");
}
