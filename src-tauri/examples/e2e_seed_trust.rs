//! Test-only helper used by `scripts/e2e-driver-smoke.mjs` to persist a real
//! repository trust record inside the E2E run's isolated config directory
//! (`XDG_CONFIG_HOME` / `APPDATA`) without adding any trust bypass to the
//! shipping `gitpulse` binary.

use std::process;

fn main() {
    let repos: Vec<String> = std::env::args().skip(1).collect();
    if repos.is_empty() || repos.iter().any(|arg| arg == "--help" || arg == "-h") {
        eprintln!("Usage: e2e_seed_trust <repo_path> [<repo_path> ...]");
        process::exit(if repos.is_empty() { 2 } else { 0 });
    }

    let mut granted = Vec::with_capacity(repos.len());
    for repo in repos {
        let preview = match gitpulse_lib::repository_trust::inspect(&repo) {
            Ok(preview) => preview,
            Err(error) => {
                eprintln!(
                    "e2e_seed_trust: inspect failed for {repo}: {}",
                    error.message()
                );
                process::exit(1);
            }
        };
        if let Err(error) = gitpulse_lib::repository_trust::grant(&repo, &preview.identity, true) {
            eprintln!(
                "e2e_seed_trust: grant failed for {repo}: {}",
                error.message()
            );
            process::exit(1);
        }
        granted.push(serde_json::json!({
            "path": preview.path,
            "common_dir": preview.common_dir,
            "identity": preview.identity,
        }));
    }

    println!(
        "{}",
        serde_json::to_string(&granted).expect("serialize trust summary")
    );
}
