//! Adversarial: report ordering through the real bridge and the real hub.
//!
//! A session's `PostToolUse` (`tool_finished`) is followed by its `Stop`
//! (`turn_finished`). The hub clears a standing state on the main agent's
//! `tool_finished`, so if the two reach `offer()` reversed the session ends up
//! standing on nothing instead of "Finished". This module is shared verbatim
//! between the base and the lane so the reorder rate can be compared.
use super::*;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

struct NullHost;

impl crate::alerts::Host for NullHost {
    fn submit(&self, _: &str, _: &str, _: &str, _: bool) -> Result<bool, String> {
        Ok(true)
    }
    fn attended(&self, _: &str) -> bool {
        false
    }
}

fn payload(event: &str, session: &str) -> Vec<u8> {
    format!(r#"{{"v":1,"event":"{event}","agent":"claude","session":"{session}"}}"#).into_bytes()
}

fn finished_stands(session: &str) -> bool {
    crate::alerts::standing()
        .iter()
        .any(|s| s.session == session && s.event.as_deref() == Some("turn_finished"))
}

/// Which way to drive the two connections.
#[derive(Clone, Copy)]
enum Shape {
    /// connect A, write A, close A, connect B, write B, close B (what two hook
    /// processes that do not overlap look like)
    Sequential,
    /// both connected first, then A then B written and half-closed back to back
    Interleaved,
    /// A and B hook processes started on separate threads released together
    /// with A released first: the harshest realistic case
    Racing,
}

fn one_trial(path: &std::path::Path, session: &str, shape: Shape) {
    let tool = payload("tool_finished", session);
    let turn = payload("turn_finished", session);
    let finish = |mut s: UnixStream, body: &[u8]| {
        let _ = s.write_all(body);
        let _ = s.shutdown(std::net::Shutdown::Write);
        s
    };
    let (a, b) = match shape {
        Shape::Sequential => {
            let a = finish(UnixStream::connect(path).unwrap(), &tool);
            let b = finish(UnixStream::connect(path).unwrap(), &turn);
            (a, b)
        }
        Shape::Interleaved => {
            let a = UnixStream::connect(path).unwrap();
            let b = UnixStream::connect(path).unwrap();
            (finish(a, &tool), finish(b, &turn))
        }
        Shape::Racing => {
            let a = UnixStream::connect(path).unwrap();
            let b = UnixStream::connect(path).unwrap();
            let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
            let g2 = gate.clone();
            let tool2 = tool.clone();
            let ta = std::thread::spawn(move || {
                g2.wait();
                finish_free(a, &tool2)
            });
            gate.wait();
            let b = finish(b, &turn);
            (ta.join().unwrap(), b)
        }
    };
    for mut s in [a, b] {
        let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        let mut reply = String::new();
        let _ = s.read_to_string(&mut reply);
    }
}

fn finish_free(mut s: UnixStream, body: &[u8]) -> UnixStream {
    let _ = s.write_all(body);
    let _ = s.shutdown(std::net::Shutdown::Write);
    s
}

/// Runs `trials` and returns how many ended without "Finished" standing.
fn reorder_count(shape: Shape, trials: usize, tag: &str) -> usize {
    let _own = own_listener();
    crate::alerts::start(std::sync::Arc::new(NullHost));
    let dir = ShortDir::new(tag);
    apply_at(true, Some(dir.socket()));
    assert!(status().listening, "{:?}", status().error);
    let mut reversed = 0;
    let mut batch: Vec<String> = Vec::new();
    for n in 0..trials {
        let session = format!("ord-{tag}-{n}");
        one_trial(&dir.socket(), &session, shape);
        batch.push(session);
        // The hub drains a bounded queue; look at the outcome in batches so a
        // slow worker is not mistaken for a reorder.
        if batch.len() == 16 || n + 1 == trials {
            for s in batch.drain(..) {
                if !within(std::time::Duration::from_millis(700), || finished_stands(&s)) {
                    reversed += 1;
                    if reversed <= 3 {
                        eprintln!("not standing: {s}; standing={:?}", crate::alerts::standing());
                    }
                }
            }
        }
    }
    apply_at(false, None);
    reversed
}

#[test]
#[ignore = "measurement: run with --ignored --nocapture"]
fn measure_reorder_rate() {
    for (name, shape) in [
        ("sequential", Shape::Sequential),
        ("interleaved", Shape::Interleaved),
        ("racing", Shape::Racing),
    ] {
        let trials = 200;
        let reversed = reorder_count(shape, trials, &name[..3]);
        eprintln!("REORDER {name}: {reversed}/{trials}");
    }
}

/// The claim a serial accept loop made implicitly: a report whose connection
/// finished first is offered first.
#[test]
fn sequential_hook_reports_from_one_session_are_never_reordered() {
    let reversed = reorder_count(Shape::Sequential, 200, "seq");
    assert_eq!(reversed, 0, "{reversed}/200 tool_finished/turn_finished pairs arrived reversed");
}

#[test]
fn interleaved_hook_reports_from_one_session_are_never_reordered() {
    let reversed = reorder_count(Shape::Interleaved, 200, "int");
    assert_eq!(reversed, 0, "{reversed}/200 pairs arrived reversed");
}
