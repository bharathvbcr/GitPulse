#!/usr/bin/env python3
"""Instruments the library's spawn gate so each integration-test binary (its exe lives in
target/debug/deps) records every spawn it makes UNDER the production spawn rate, i.e. before (or
without) common::unmetered_spawns()/run_process_with_unlimited_spawn_rate(). One line per spawn:
`<suite exe> | <test thread name> | <label>` appended to $L4_METERED_LOG (default below).

Any suite that appears in the log is a suite whose in-process spawns still run metered: if it is on
the lane's UNMETERED_ALLOWLIST with a "no git spawn" reason, the reason is false; if it is a switched
suite, an entry point reaches the gate before the switch. Built binaries (target/debug/<bin>) are not
test crates and are skipped. Revert with `git checkout -- src-tauri/src/engine/git_cli.rs`.
"""
import sys, pathlib

p = pathlib.Path(sys.argv[1]) / "src-tauri" / "src" / "engine" / "git_cli.rs"
s = p.read_text()
old = """        let rate_limited = self.refill_per_sec != u32::MAX
            && !PROCESS_UNLIMITED_RATE.load(std::sync::atomic::Ordering::Relaxed);
"""
new = old + """        if rate_limited {
            if let Ok(exe) = std::env::current_exe() {
                if exe.parent().is_some_and(|dir| dir.ends_with("deps")) {
                    use std::io::Write;
                    let log = std::env::var("L4_METERED_LOG").unwrap_or_else(|_| {
                        "/Users/bharath/Code/devtools/GitPulse/tmp/build-1026/l4-adv/metered.log".into()
                    });
                    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
                        let _ = writeln!(
                            file,
                            "{} | {} | {:?}",
                            exe.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                            std::thread::current().name().unwrap_or("?"),
                            class
                        );
                    }
                }
            }
        }
"""
assert s.count(old) == 1
p.write_text(s.replace(old, new))
print("metered log applied")
