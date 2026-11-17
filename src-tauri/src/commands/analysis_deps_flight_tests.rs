use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn failing() -> Result<DepsHealthReport, CommandError> {
    Err(CommandError::from("scan stub".to_string()))
}

/// The defect: every caller ran its own full audit of the repository.
#[test]
fn concurrent_scans_of_one_repository_share_one_audit() {
    let flights = Arc::new(DepsFlights::default());
    let runs = Arc::new(AtomicUsize::new(0));
    let leader = {
        let (flights, runs) = (Arc::clone(&flights), Arc::clone(&runs));
        std::thread::spawn(move || {
            scan_deps_once(&flights, "/repo", || {
                runs.fetch_add(1, Ordering::SeqCst);
                // Wait for the second caller to either join this scan or (the
                // defect) start a scan of its own, so a regression fails the
                // count below instead of hanging here.
                crate::test_support::wait_until(Duration::from_secs(30), || {
                    flights.followers(&"/repo".to_string()) > 0 || runs.load(Ordering::SeqCst) > 1
                });
                failing()
            })
        })
    };
    assert!(
        crate::test_support::wait_until(Duration::from_secs(30), || {
            runs.load(Ordering::SeqCst) > 0
        }),
        "the leader's scan never started"
    );
    let follower = scan_deps_once(&flights, "/repo", || {
        runs.fetch_add(1, Ordering::SeqCst);
        failing()
    });
    assert!(follower.is_err());
    assert!(leader.join().unwrap().is_err());
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[test]
fn different_repositories_scan_independently() {
    let flights = DepsFlights::default();
    let runs = AtomicUsize::new(0);
    for repo in ["/a", "/b"] {
        let _ = scan_deps_once(&flights, repo, || {
            runs.fetch_add(1, Ordering::SeqCst);
            failing()
        });
    }
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}
