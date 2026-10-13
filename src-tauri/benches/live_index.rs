//! Live-index duty cycle under a synthetic edit storm.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench live_index`.
//!
//! The live index rebuilds the code graph off the watcher's `repo-changed`
//! ticks. If every tick of an edit storm starts a build, the indexer is busy
//! for as long as the storm lasts, and the machine pays for an index nobody is
//! looking at. The audit card `live-index-duty-cycle` is about bounding that;
//! this is its measurement.
//!
//! # How
//!
//! Only public seams: `devmap::live::maybe_refresh(repo, repo_changed)` is
//! called the way the watcher's handler calls it, once per tick, for the length
//! of the storm. The `devmap` binary is a stub shell script
//! (`GITPULSE_DEVMAP_BIN`) that answers `status --json` as a fresh, current
//! schema and `build` after a fixed delay with `{"ok":true,"unchanged":false}`,
//! logging each invocation to a file. The code graph's consumer artifacts are
//! pre-created so a build is not treated as the restore of a missing artifact.
//! The stub is deterministic, so the numbers are the gating logic's behaviour,
//! not a real indexer's speed; a real build is longer, which only makes the
//! duty cycle worse.
//!
//! `maybe_refresh` blocks while its build runs, so the time spent inside calls
//! that started a build, over the storm's wall time, is the **duty cycle**: the
//! fraction of the storm during which an index build was running. Reported
//! beside it: builds started, `devmap` invocations by kind, and how many ticks
//! ended in each decision. The tick cadence, storm length and stub build time
//! are fixed and printed. All metrics are rates of a wall-clock process, so the
//! row is not time-gated; its counters (ticks) are.

#[cfg(unix)]
mod common;

#[cfg(unix)]
mod imp {
    use super::common::fixture::{build_repo, Spec};
    use super::common::{interleave, Case, Report, Sample};
    use gitpulse_lib::devmap::live::{clear_live_echo_cooldown, maybe_refresh};
    use gitpulse_lib::devmap::repo_map::repo_map_path;
    use gitpulse_lib::devmap::viz::code_graph_path;
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    /// How long the stub's `build` takes: a short incremental build.
    const BUILD_MS: u64 = 300;
    /// Watcher debounce cadence during the storm.
    const TICK: Duration = Duration::from_millis(250);

    pub fn main() {
        gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
        let mut report = Report::new("live_index");
        // The same storm in smoke and full mode (only the sample count differs),
        // so the row name and its `ticks` counter match the baseline's.
        let ticks = 40;
        let temp = tempfile::tempdir().expect("temp dir");
        let spec = Spec {
            files: 40,
            dirs: 4,
            extra_commits: 2,
            touch: 2,
            linked_worktrees: 0,
        };
        let repo = build_repo(&temp.path().join("fixture"), &spec);
        let path = repo.path().to_string();

        for artifact in [repo_map_path(&repo.main), code_graph_path(&repo.main)] {
            std::fs::create_dir_all(artifact.parent().expect("artifact parent")).expect("mkdir");
            std::fs::write(&artifact, "{}").expect("pre-create the consumer artifact");
        }

        let log = temp.path().join("devmap-invocations.log");
        let stub = temp.path().join("devmap");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\n\
                 echo \"$1\" >> '{log}'\n\
                 case \"$1\" in\n\
                 status) echo '{{\"is_fresh\":true,\"schema_outdated\":false}}' ;;\n\
                 build) sleep 0.{BUILD_MS:03}; echo '{{\"ok\":true,\"unchanged\":false}}' ;;\n\
                 *) echo '{{}}' ;;\n\
                 esac\n",
                log = log.display()
            ),
        )
        .expect("write the stub devmap");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        std::env::set_var("GITPULSE_DEVMAP_BIN", &stub);

        report.note(format!(
            "stub devmap (status = fresh, build = {BUILD_MS} ms), one repo-changed tick every {} ms for {ticks} ticks ({} s storm)",
            TICK.as_millis(),
            ticks as u64 * TICK.as_millis() as u64 / 1000
        ));

        let storm = || -> Sample {
            clear_live_echo_cooldown(&path);
            let _ = std::fs::remove_file(&log);
            let mut decisions: BTreeMap<String, u64> = BTreeMap::new();
            let mut building = Duration::ZERO;
            let started = Instant::now();
            for tick in 0..ticks {
                let due = started + TICK * tick as u32;
                if let Some(wait) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                let call = Instant::now();
                let outcome = maybe_refresh(&path, true);
                let took = call.elapsed();
                if outcome.build.is_some() {
                    building += took;
                }
                *decisions
                    .entry(format!("{:?}", outcome.decision))
                    .or_default() += 1;
            }
            let wall = started.elapsed();
            let invocations = std::fs::read_to_string(&log).unwrap_or_default();
            let count = |kind: &str| invocations.lines().filter(|l| *l == kind).count() as f64;
            let mut sample = Sample::default()
                .counter("ticks", ticks as u64)
                .metric("duty_cycle", building.as_secs_f64() / wall.as_secs_f64())
                .metric("builds_started", count("build"))
                .metric("status_probes", count("status"));
            for (decision, n) in &decisions {
                // Static names for the few decisions that exist.
                let name: &'static str =
                    Box::leak(format!("ticks_{}", decision.to_lowercase()).into_boxed_str());
                sample = sample.metric(name, *n as f64);
            }
            sample.elapsed(wall)
        };

        let rows = interleave(
            vec![Case::new(format!("edit storm {ticks} ticks"), storm).ungated()],
            report.opts.samples(3),
            0,
        );
        report.extend(rows);
        report.finish();
    }
}

#[cfg(unix)]
fn main() {
    imp::main();
}

#[cfg(not(unix))]
fn main() {
    println!("the live-index bench drives a shell-script devmap stub; nothing to measure here");
}
