//! One run of a slow call per key, however many callers ask at once.
//!
//! The caches that sit in front of a subprocess or a sidecar round trip used
//! to hold their own lock across it, which serialised *every* key behind the
//! slowest one (an `agy models` call is 20 s), or held no lock and ran the
//! same probe once per caller. A [`SingleFlight`] gives a caller that arrives
//! while the same key is already running the leader's answer, and lets other
//! keys, and the cache itself, proceed untouched.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Condvar, Mutex};

use crate::supervisor::lock_or_recover;

struct Flight<V> {
    answer: Mutex<Option<V>>,
    done: Condvar,
    /// Callers that joined this run instead of making their own, counted when
    /// they take the flight under the map lock. Lets a test wait until a
    /// follower has really joined rather than sleep and assume it did.
    #[cfg(test)]
    followers: std::sync::atomic::AtomicUsize,
}

pub struct SingleFlight<K, V> {
    running: Mutex<HashMap<K, Arc<Flight<V>>>>,
}

impl<K, V> Default for SingleFlight<K, V> {
    fn default() -> Self {
        Self {
            running: Mutex::new(HashMap::new()),
        }
    }
}

/// Retires the leader's flight however it ends. A leader that panics leaves
/// no answer, and its followers then run the call themselves rather than hang.
struct Leader<'a, K: Hash + Eq, V> {
    owner: &'a SingleFlight<K, V>,
    key: Option<K>,
    flight: Arc<Flight<V>>,
}

impl<K: Hash + Eq, V> Drop for Leader<'_, K, V> {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            lock_or_recover(&self.owner.running).remove(&key);
        }
        self.flight.done.notify_all();
    }
}

#[cfg(test)]
impl<K: Hash + Eq, V> SingleFlight<K, V> {
    /// How many callers have joined the run now in flight for `key`; 0 when
    /// none is running.
    pub fn followers(&self, key: &K) -> usize {
        lock_or_recover(&self.running).get(key).map_or(0, |flight| {
            flight.followers.load(std::sync::atomic::Ordering::SeqCst)
        })
    }
}

impl<K: Hash + Eq + Clone, V: Clone> SingleFlight<K, V> {
    /// Runs `call` unless the same `key` is already running, in which case
    /// this waits for that run and returns its answer.
    pub fn run(&self, key: K, call: impl FnOnce() -> V) -> V {
        let existing = {
            let mut running = lock_or_recover(&self.running);
            match running.get(&key) {
                Some(flight) => {
                    #[cfg(test)]
                    flight
                        .followers
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Some(Arc::clone(flight))
                }
                None => {
                    running.insert(
                        key.clone(),
                        Arc::new(Flight {
                            answer: Mutex::new(None),
                            done: Condvar::new(),
                            #[cfg(test)]
                            followers: std::sync::atomic::AtomicUsize::new(0),
                        }),
                    );
                    None
                }
            }
        };
        if let Some(flight) = existing {
            let mut answer = lock_or_recover(&flight.answer);
            // Woken with an answer, or by a leader that ended without one.
            // The flight leaves the map before the wake, so a second wait on
            // it would never end.
            while answer.is_none() && lock_or_recover(&self.running).contains_key(&key) {
                answer = flight
                    .done
                    .wait_timeout(answer, std::time::Duration::from_millis(50))
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0;
            }
            if let Some(answer) = answer.clone() {
                return answer;
            }
            drop(answer);
            return self.run(key, call);
        }
        let flight = lock_or_recover(&self.running)
            .get(&key)
            .map(Arc::clone)
            .expect("the leader inserted its own flight");
        let leader = Leader {
            owner: self,
            key: Some(key),
            flight: Arc::clone(&flight),
        };
        let value = call();
        *lock_or_recover(&flight.answer) = Some(value.clone());
        drop(leader);
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    #[test]
    fn concurrent_callers_for_one_key_share_one_run() {
        let flights = Arc::new(SingleFlight::<&'static str, usize>::default());
        let runs = Arc::new(AtomicUsize::new(0));
        let started = Instant::now();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (flights, runs) = (flights.clone(), runs.clone());
                std::thread::spawn(move || {
                    flights.run("k", || {
                        std::thread::sleep(Duration::from_millis(300));
                        runs.fetch_add(1, Ordering::SeqCst) + 1
                    })
                })
            })
            .collect();
        let answers: Vec<usize> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "the call ran more than once"
        );
        assert!(answers.iter().all(|a| *a == 1), "{answers:?}");
        assert!(started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(2)));
    }

    #[test]
    fn another_key_is_not_held_behind_a_slow_one() {
        let flights = Arc::new(SingleFlight::<&'static str, u8>::default());
        let slow = {
            let flights = flights.clone();
            std::thread::spawn(move || {
                flights.run("slow", || {
                    std::thread::sleep(Duration::from_millis(800));
                    1
                })
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        let started = Instant::now();
        assert_eq!(flights.run("fast", || 2), 2);
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_millis(400))
        );
        assert_eq!(slow.join().unwrap(), 1);
    }

    #[test]
    fn a_leader_that_panics_does_not_strand_its_followers() {
        let flights = Arc::new(SingleFlight::<&'static str, u8>::default());
        let leader = {
            let flights = flights.clone();
            std::thread::spawn(move || {
                flights.run("k", || {
                    std::thread::sleep(Duration::from_millis(200));
                    panic!("injected panic in the leader")
                })
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        let follower = flights.run("k", || 7);
        assert_eq!(follower, 7);
        assert!(leader.join().is_err());
    }
}
