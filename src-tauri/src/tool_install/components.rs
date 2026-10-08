//! What of DevCouncil is actually installed on this machine.
//!
//! [`super::ExternalTool`] covers the two binaries GitPulse *manages* — it can
//! install, verify, disable and uninstall `devmap` and `manvi`. That is a
//! smaller set than the one the setup wizard offers to install: its "analysis
//! suite" preset installs `dcstore`, `dcverify` and `dcgrep`, and its full
//! preset adds the Go host. Nothing probed those afterwards, so the wizard
//! reported success from a shell exit code and the app could not say whether
//! the components arrived, where they landed, or whether they still run.
//!
//! That mattered beyond tidiness. `manvi serve --workbench-db <profile>` — the
//! exact invocation `harness::sidecar` spawns for the profile workbench —
//! resolves `dcstore` from `PATH` (Manvi's `MANVI_STORE_BINARY`, defaulting to
//! the bare name). So a missing `dcstore` breaks managed agent runs and
//! enhancements, and until now it did so with no way to see why.
//!
//! A present one is not enough either. `dcstore --version` names the workbench
//! schema the build opens (`workbench_schema`), and only one that opens the
//! schema GitPulse's vendored store opens is reported installed: a stale
//! `dcstore` first on `PATH` is the one Manvi picks, and it refuses the
//! profile. [`verified_store_binary`] is that checked binary, which the
//! sidecar hands Manvi as `MANVI_STORE_BINARY`.
//!
//! ## Presence, and only the version that exists
//!
//! DevCouncil components (`devmap`, `manvi`, `dcstore`, `dcverify`, `dcgrep`,
//! and the Go host) answer version requests. The analysis plane binaries
//! (`dcstore`, `dcverify`, `dcgrep`) report component identity and universal
//! product version as structured JSON over `--version`. GitPulse validates the
//! reported component identity against impostor binaries and wires the
//! version number into the suite inventory. For legacy builds that do not yet
//! expose `--version`, presence is verified via read-only handshakes and reported
//! as `NotExposed`. Presence is established by running the binary, not by stat-ing
//! a path: a file named `dcstore` that cannot execute is not an installed component.
//!
//! Every probe runs in a scratch directory, never in a user repository, and is
//! chosen to have no side effects.

use super::ExternalTool;
use crate::engine::git_cli;
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::time::Duration;

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_CAP: usize = 64 * 1024;

/// How much GitPulse depends on one component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentNeed {
    /// GitPulse spawns it itself; a named feature stops working without it.
    Required,
    /// GitPulse does not spawn it, but the Manvi host it starts resolves it
    /// from `PATH` while serving requests GitPulse makes.
    HostResolved,
    /// Neither GitPulse nor the host it starts needs it. Installed by a
    /// preset for use outside this application.
    Optional,
}

/// How a component's version was established, or why it could not be.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum VersionReading {
    /// The binary reported this.
    Reported { version: String },
    /// The binary ran, but exposes no way to ask its version.
    NotExposed { detail: String },
    /// The binary could not be run.
    Unavailable { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentStatus {
    /// Executable name, as it is looked up.
    pub id: String,
    pub label: String,
    pub need: ComponentNeed,
    /// One sentence: what stops working without it.
    pub purpose: String,
    /// The binary was found *and* ran.
    pub installed: bool,
    pub path: Option<String>,
    pub version: VersionReading,
    /// Why it is not installed, when it is not.
    pub reason: Option<String>,
    /// Preset ids from the setup wizard that install this component.
    pub presets: Vec<String>,
}

/// One install preset measured against what is on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetStatus {
    pub id: String,
    /// Components this preset installs that are present and runnable.
    pub present: usize,
    pub total: usize,
    /// Component ids this preset claims to install that are missing.
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuiteStatus {
    pub components: Vec<ComponentStatus>,
    pub presets: Vec<PresetStatus>,
    /// True when every `Required` and `HostResolved` component is present.
    /// Optional components never make this false.
    pub complete: bool,
}

/// How to establish that one component is really installed.
#[derive(Clone, Copy)]
enum Probe {
    /// Run with `--version` and read the first line naming a version.
    VersionFlag,
    /// Run with `--version` first. If that fails (e.g. an older release that
    /// rejected `--version`), fall back to the given arguments.
    VersionWithFallback(&'static [&'static str]),
    /// Run the given arguments and require a JSON object on stdout. Used for
    /// the components with no version flag; the arguments are chosen to be
    /// read-only and to need no repository.
    ///
    /// No catalogue entry constructs this yet — `evaluate_json_response` and
    /// the tests around it are reachable, but the library build alone sees the
    /// variant as dead. `expect` rather than `allow` deliberately: the moment
    /// a spec does construct it, the expectation goes unfulfilled and this
    /// attribute asks to be deleted, instead of quietly outliving its reason.
    #[cfg_attr(not(test), expect(dead_code))]
    JsonHandshake(&'static [&'static str]),
}

struct Spec<'a> {
    id: &'a str,
    label: &'a str,
    need: ComponentNeed,
    purpose: &'a str,
    probe: Probe,
    presets: &'static [&'static str],
    /// Managed tools resolve through env → saved config → PATH; the rest are
    /// looked up on `PATH` and GitPulse's own bin directory only.
    managed: Option<ExternalTool>,
    /// The variable the Manvi host reads this component's path from before
    /// it searches `PATH`. When it is set, that binary is the one the host
    /// runs, so it is the one probed.
    host_env: Option<&'static str>,
    /// The workbench schema this component must report opening
    /// (`workbench_schema` in its `--version` answer) to count as installed.
    workbench_schema: Option<i64>,
}

/// The catalogue. Kept beside the presets in `devcouncilInstall.ts`, which is
/// what offers to install these in the first place.
fn catalogue() -> Vec<Spec<'static>> {
    vec![
        Spec {
            id: "devmap",
            label: "DevMap",
            need: ComponentNeed::Required,
            purpose: "Builds and refreshes the code index behind Code → Map, search, impact and affected tests.",
            probe: Probe::VersionFlag,
            presets: &["devmap", "analysis", "all"],
            managed: Some(ExternalTool::Devmap),
            host_env: None,
            workbench_schema: None,
        },
        Spec {
            id: "manvi",
            label: "Manvi",
            need: ComponentNeed::Required,
            purpose: "Hosts policy, the profile workbench and managed agent runs.",
            probe: Probe::VersionFlag,
            presets: &["all"],
            managed: Some(ExternalTool::Manvi),
            host_env: None,
            workbench_schema: None,
        },
        Spec {
            id: "dcstore",
            label: "dcstore",
            need: ComponentNeed::HostResolved,
            purpose: "Manvi opens the profile workbench through it; without it managed runs and enhancements fail.",
            probe: Probe::VersionWithFallback(&[]),
            presets: &["analysis", "all"],
            managed: None,
            host_env: Some("MANVI_STORE_BINARY"),
            workbench_schema: Some(crate::workbench::WORKBENCH_SCHEMA),
        },
        Spec {
            id: "dcverify",
            label: "dcverify",
            need: ComponentNeed::HostResolved,
            purpose: "Manvi's verification gate. GitPulse links the same library, so its own reads work without the binary.",
            probe: Probe::VersionWithFallback(&[]),
            presets: &["analysis", "all"],
            managed: None,
            host_env: Some("MANVI_VERIFY_BINARY"),
            workbench_schema: None,
        },
        Spec {
            id: "dcgrep",
            label: "dcgrep",
            need: ComponentNeed::HostResolved,
            purpose: "Manvi's repository search. GitPulse does not spawn it directly.",
            probe: Probe::VersionWithFallback(&["health"]),
            presets: &["analysis", "all"],
            managed: None,
            host_env: None,
            workbench_schema: None,
        },
        Spec {
            id: "devcouncil",
            label: "DevCouncil host",
            need: ComponentNeed::Optional,
            purpose: "The Go orchestrator. GitPulse talks to Manvi instead and never starts this.",
            probe: Probe::VersionFlag,
            presets: &["all"],
            managed: None,
            host_env: None,
            workbench_schema: None,
        },
    ]
}

/// A directory no user owns, so a probe can never read a repository.
fn probe_dir() -> std::path::PathBuf {
    std::env::temp_dir()
}

fn run_probe(path: &str, id: &str, args: &[&str]) -> Result<(bool, String), String> {
    let mut cmd = Command::new(path);
    cmd.args(args);
    cmd.current_dir(probe_dir());
    let run = git_cli::run_bounded_capped(cmd, id, PROBE_TIMEOUT, None, PROBE_CAP)
        .map_err(|e| e.to_string())?
        .require_complete(id)?;
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();
    let combined = if stdout.trim().is_empty() {
        stderr
    } else {
        stdout
    };
    Ok((run.success, combined))
}

/// Resolve one component's path without running it.
fn locate(spec: &Spec<'_>) -> Option<String> {
    if let Some(tool) = spec.managed {
        // Managed tools honour the explicit override and the saved config, so
        // reporting a PATH copy while the app runs a configured one elsewhere
        // would be a lie about which binary answers.
        return super::resolve_status(tool).path;
    }
    let host_choice = spec
        .host_env
        .and_then(std::env::var_os)
        .filter(|choice| !choice.is_empty());
    host_located(spec.id, host_choice.as_deref())
}

/// Where the Manvi host finds `id`: the path its override variable names, as
/// given, or a bare name there or `id` itself looked up — the same rule
/// Manvi's own resolution follows, so the binary probed is the one it runs.
fn host_located(id: &str, host_choice: Option<&std::ffi::OsStr>) -> Option<String> {
    match host_choice {
        Some(choice) if std::path::Path::new(choice).components().count() > 1 => {
            Some(choice.to_string_lossy().into_owned())
        }
        Some(choice) => git_cli::find_external_tool(&choice.to_string_lossy()),
        None => git_cli::find_external_tool(id),
    }
}

/// The `dcstore` the Manvi host should open the profile workbench through:
/// located as the host would find it, and probed as the inventory probes it —
/// its identity, and that it opens the workbench schema GitPulse's vendored
/// store opens. `None` when no such binary is installed.
pub(crate) fn verified_store_binary() -> Option<String> {
    let spec = catalogue().into_iter().find(|spec| spec.id == "dcstore")?;
    let status = probe(&spec);
    if status.installed {
        status.path
    } else {
        None
    }
}

fn probe(spec: &Spec<'_>) -> ComponentStatus {
    let located = locate(spec);
    probe_located(spec, located)
}

/// Matches the identity reported in a component's JSON response against the expected tool id.
fn matches_component_identity(reported: &str, expected_id: &str) -> bool {
    let rep = reported.trim().to_ascii_lowercase();
    let exp = expected_id.trim().to_ascii_lowercase();
    if rep == exp {
        return true;
    }
    if rep.replace(['-', '_'], "") == exp.replace(['-', '_'], "") {
        return true;
    }
    match exp.as_str() {
        "devcouncil" => rep == "host" || rep == "go-host",
        "devmap" => rep == "devmap-cli",
        _ => false,
    }
}

enum JsonProbeOutcome {
    Reported { version: String },
    NotExposed { detail: String },
    IdentityMismatch { reported: String },
    Malformed { detail: String },
}

fn evaluate_json_response(value: &serde_json::Value, spec_id: &str) -> JsonProbeOutcome {
    let Some(obj) = value.as_object() else {
        return JsonProbeOutcome::Malformed {
            detail: "response was not a JSON object".into(),
        };
    };

    let identity_keys = ["id", "component", "store", "verifier", "searcher"];
    let mut found_identities = Vec::new();
    for key in identity_keys {
        if let Some(val) = obj.get(key).and_then(serde_json::Value::as_str) {
            found_identities.push(val);
        }
    }

    if !found_identities.is_empty() {
        let any_match = found_identities
            .iter()
            .any(|id| matches_component_identity(id, spec_id));
        if !any_match {
            return JsonProbeOutcome::IdentityMismatch {
                reported: found_identities[0].to_string(),
            };
        }
    }

    if let Some(version) = obj.get("version").and_then(serde_json::Value::as_str) {
        let trimmed = version.trim();
        if !trimmed.is_empty() {
            return JsonProbeOutcome::Reported {
                version: trimmed.to_string(),
            };
        }
    }

    JsonProbeOutcome::NotExposed {
        detail: "this component exposes no version flag".into(),
    }
}

/// What running a component told us: whether it exited zero, and the text a
/// reader should interpret — or why it could not be run at all.
type ProbeAnswer = Result<(bool, String), String>;

/// The probe itself, with lookup already done.
///
/// Split out so a test can drive a real executable at a known path: passing a
/// path through `Spec::id` instead would make every fixture resolve to "not
/// found", and a test asserting "not installed" would pass without ever
/// running the binary it claims to reject.
fn probe_located(spec: &Spec<'_>, located: Option<String>) -> ComponentStatus {
    probe_located_with(spec, located, &mut |path, id, args| {
        run_probe(path, id, args)
    })
}

/// [`probe_located`] with the invocation injectable.
///
/// Everything below the `run` calls is a decision about an exit status and one
/// string, and the identity rules it enforces are the security-relevant part:
/// an impostor must not be reported as installed, and a legacy build that
/// cannot answer `--version` must not be reported as absent. Reaching those
/// branches through a `#!/bin/sh` stub made each of them wait on a child
/// reaching `main` inside [`PROBE_TIMEOUT`], which is a measurement of host
/// load. A fake runner also reaches answers a stub cannot give reliably — a
/// probe that timed out, output on the stream the reader does not prefer.
///
/// `git_cli::run_bounded_capped` owns the other half: that a real child runs,
/// and that its status and output come back.
fn probe_located_with(
    spec: &Spec<'_>,
    located: Option<String>,
    run: &mut dyn FnMut(&str, &str, &[&str]) -> ProbeAnswer,
) -> ComponentStatus {
    let base = |installed: bool,
                path: Option<String>,
                version: VersionReading,
                reason: Option<String>| ComponentStatus {
        id: spec.id.to_string(),
        label: spec.label.to_string(),
        need: spec.need,
        purpose: spec.purpose.to_string(),
        installed,
        path,
        version,
        reason,
        presets: spec.presets.iter().map(|p| p.to_string()).collect(),
    };

    let Some(path) = located else {
        return base(
            false,
            None,
            VersionReading::Unavailable {
                detail: "not found".into(),
            },
            Some(format!("`{}` is not on PATH", spec.id)),
        );
    };

    // Every JSON answer is judged the same way, whichever call produced it.
    let impostor = |reported: String| {
        base(
            false,
            Some(path.clone()),
            VersionReading::Unavailable {
                detail: format!("reported unexpected component identity {reported:?}"),
            },
            Some(format!(
                "`{}` did not answer like a DevCouncil component: reported identity {reported:?}",
                spec.id
            )),
        )
    };
    let unlike = |detail: String| {
        base(
            false,
            Some(path.clone()),
            VersionReading::Unavailable {
                detail: detail.clone(),
            },
            Some(format!(
                "`{}` did not answer like a DevCouncil component: {detail}",
                spec.id
            )),
        )
    };
    // The component answered as itself. One held to a workbench schema is
    // installed only if it said it opens that schema; `answer` is `None`
    // when it said nothing a schema could be read from.
    let accept = |version: VersionReading, answer: Option<&serde_json::Value>| {
        let Some(required) = spec.workbench_schema else {
            return base(true, Some(path.clone()), version, None);
        };
        let reported = answer
            .and_then(|answer| answer.get("workbench_schema"))
            .and_then(serde_json::Value::as_i64);
        let reason = match reported {
            Some(reported) if reported == required => {
                return base(true, Some(path.clone()), version, None);
            }
            Some(reported) if reported < required => format!(
                "`{path}` opens workbench schema {reported}, and GitPulse's task board is schema {required}. Manvi runs this `{}` against the board and it refuses the profile: update it to one that opens schema {required}.",
                spec.id
            ),
            Some(reported) => format!(
                "`{path}` opens workbench schema {reported}, newer than GitPulse's task board (schema {required}); Manvi running it would migrate the profile past what GitPulse reads. Install a `{}` that opens schema {required}, or update GitPulse.",
                spec.id
            ),
            None => format!(
                "`{path}` does not report the workbench schema it opens (`workbench_schema` in `{} --version`), so it predates the check for schema {required}, the one GitPulse's task board needs. Manvi runs it against the board: update it.",
                spec.id
            ),
        };
        base(false, Some(path.clone()), version, Some(reason))
    };
    let judge = |value: &serde_json::Value| match evaluate_json_response(value, spec.id) {
        JsonProbeOutcome::Reported { version } => {
            accept(VersionReading::Reported { version }, Some(value))
        }
        JsonProbeOutcome::NotExposed { detail } => {
            accept(VersionReading::NotExposed { detail }, Some(value))
        }
        JsonProbeOutcome::IdentityMismatch { reported } => impostor(reported),
        JsonProbeOutcome::Malformed { detail } => unlike(detail),
    };
    let json_object = |text: &str| {
        serde_json::from_str::<serde_json::Value>(text.trim())
            .ok()
            .filter(serde_json::Value::is_object)
    };

    match spec.probe {
        Probe::VersionFlag | Probe::VersionWithFallback(_) => {
            let fallback_args = match spec.probe {
                Probe::VersionWithFallback(args) => Some(args),
                _ => None,
            };
            let asked = run(&path, spec.id, &["--version"]);
            // A refused or failed `--version` on an older release: its
            // read-only handshake, when it has one, is the evidence instead.
            let mut fallback = || {
                fallback_args
                    .and_then(|args| run(&path, spec.id, args).ok())
                    .and_then(|(_, text)| json_object(&text))
            };

            match asked {
                Ok((true, text)) => {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text.trim()) {
                        judge(&value)
                    } else {
                        let reading = match super::version_line(&text, spec.id) {
                            Some(version) => VersionReading::Reported { version },
                            None => VersionReading::NotExposed {
                                detail: "`--version` printed nothing this reader recognized".into(),
                            },
                        };
                        accept(reading, None)
                    }
                }
                Ok((false, text)) => {
                    if let Some(value) = json_object(&text) {
                        return judge(&value);
                    }
                    if let Some(value) = fallback() {
                        return judge(&value);
                    }
                    let detail = text.trim().chars().take(200).collect::<String>();
                    base(
                        false,
                        Some(path.clone()),
                        VersionReading::Unavailable {
                            detail: detail.clone(),
                        },
                        Some(format!("`{} --version` failed: {detail}", spec.id)),
                    )
                }
                Err(detail) => {
                    if let Some(value) = fallback() {
                        return judge(&value);
                    }
                    base(
                        false,
                        Some(path.clone()),
                        VersionReading::Unavailable {
                            detail: detail.clone(),
                        },
                        Some(format!("could not run `{}`: {detail}", spec.id)),
                    )
                }
            }
        }
        // These exit 0 with a JSON object whether or not the request itself
        // succeeded, so the object — not the exit status — is the evidence
        // that the binary is the component it is named after.
        Probe::JsonHandshake(args) => match run(&path, spec.id, args) {
            Ok((_, text)) => match json_object(&text) {
                Some(value) => judge(&value),
                None => unlike(text.trim().chars().take(200).collect::<String>()),
            },
            Err(detail) => base(
                false,
                Some(path.clone()),
                VersionReading::Unavailable {
                    detail: detail.clone(),
                },
                Some(format!("could not run `{}`: {detail}", spec.id)),
            ),
        },
    }
}

/// Probe every DevCouncil component and measure the install presets against
/// the result.
pub fn suite_status() -> SuiteStatus {
    let specs = catalogue();
    let components: Vec<ComponentStatus> = specs.iter().map(probe).collect();

    let mut preset_ids: Vec<String> = Vec::new();
    for component in &components {
        for preset in &component.presets {
            if !preset_ids.contains(preset) {
                preset_ids.push(preset.clone());
            }
        }
    }
    let presets = preset_ids
        .into_iter()
        .map(|id| {
            let members: Vec<&ComponentStatus> = components
                .iter()
                .filter(|c| c.presets.contains(&id))
                .collect();
            PresetStatus {
                id,
                present: members.iter().filter(|c| c.installed).count(),
                total: members.len(),
                missing: members
                    .iter()
                    .filter(|c| !c.installed)
                    .map(|c| c.id.clone())
                    .collect(),
            }
        })
        .collect();

    let complete = components
        .iter()
        .all(|component| component.installed || matches!(component.need, ComponentNeed::Optional));

    SuiteStatus {
        components,
        presets,
        complete,
    }
}

/// The component inventory together with the installation warnings `devmap
/// doctor` measures.
///
/// Two independent axes, deliberately not merged: the inventory answers "is it
/// here", doctor answers "is the one that is here the one that answers". A
/// complete inventory with a binary-skew warning is a real and common state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteReport {
    /// The inventory. Nested rather than flattened: the two axes are
    /// independent, and a flattened envelope is not comparable field-for-field
    /// by the IPC type contract.
    pub suite: SuiteStatus,
    /// Absent when there was no trusted repository to run the probe in — which
    /// is not the same as a probe that ran and found nothing wrong.
    pub doctor: Option<crate::devmap::DoctorReport>,
    /// Why `doctor` is absent, when it is.
    pub doctor_reason: Option<String>,
    /// Flattened warnings, most consequential first. Empty when `doctor` ran
    /// and found none; never populated when it did not run.
    pub warnings: Vec<String>,
}

/// Probe the suite, and ask `devmap doctor` in `repo_path` when one is given.
pub fn suite_report(repo_path: Option<&str>) -> SuiteReport {
    let suite = suite_status();
    let Some(repo) = repo_path.map(str::trim).filter(|path| !path.is_empty()) else {
        return SuiteReport {
            suite,
            doctor: None,
            doctor_reason: Some(
                "no repository is open, so installation health was not checked".into(),
            ),
            warnings: Vec::new(),
        };
    };
    let report = crate::devmap::doctor(repo);
    if !report.available {
        let reason = report
            .reason
            .clone()
            .unwrap_or_else(|| "devmap doctor did not answer".into());
        return SuiteReport {
            suite,
            doctor: Some(report),
            doctor_reason: Some(reason),
            warnings: Vec::new(),
        };
    }
    let warnings = report.warnings();
    SuiteReport {
        suite,
        doctor: Some(report),
        doctor_reason: None,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_component_belongs_to_at_least_one_preset() {
        // A component the wizard cannot install is one a user can only be told
        // about, never given.
        for spec in catalogue() {
            assert!(
                !spec.presets.is_empty(),
                "{} is probed but no preset installs it",
                spec.id
            );
        }
    }

    #[test]
    fn the_catalogue_matches_the_presets_the_wizard_offers() {
        // `devcouncilInstall.ts` is the other half of this contract: a preset
        // named here that it does not offer is a status for an install path
        // that does not exist.
        let offered = ["devmap", "analysis", "all"];
        for spec in catalogue() {
            for preset in spec.presets {
                assert!(
                    offered.contains(preset),
                    "{} names preset `{preset}`, which the wizard does not offer",
                    spec.id
                );
            }
        }
    }

    /// `manvi --version` prints its name on the first line and its version on
    /// the second. A reader that accepted either match reported the version as
    /// the literal string `manvi` — a probe that looks like it worked.
    #[test]
    fn a_version_block_is_read_past_its_banner() {
        let manvi = "manvi\n  version    v0.0.5 (-ldflags)\n  revision   b845e0b (modified)\n";
        assert_eq!(
            super::super::version_line(manvi, "manvi").as_deref(),
            Some("version v0.0.5 (-ldflags)")
        );
        // A single-line banner with no `version` word still reads correctly.
        assert_eq!(
            super::super::version_line("devmap 0.2.1 (store schema 20)\n", "devmap").as_deref(),
            Some("devmap 0.2.1 (store schema 20)")
        );
        // Nothing recognizable is None, never a fabricated string.
        assert_eq!(super::super::version_line("segfault\n", "devmap"), None);
        assert_eq!(super::super::version_line("", "devmap"), None);
    }

    #[test]
    fn component_ids_are_unique() {
        let mut ids: Vec<&str> = catalogue().iter().map(|spec| spec.id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate component id");
    }

    #[test]
    fn a_missing_component_is_reported_missing_not_versionless() {
        let spec = Spec {
            id: "gitpulse-component-that-does-not-exist",
            label: "absent",
            need: ComponentNeed::Optional,
            purpose: "test",
            probe: Probe::VersionFlag,
            presets: &["all"],
            managed: None,
            host_env: None,
            workbench_schema: None,
        };
        let status = probe(&spec);
        assert!(!status.installed);
        assert!(status.path.is_none());
        assert!(matches!(status.version, VersionReading::Unavailable { .. }));
        assert!(status.reason.is_some(), "a missing tool must say so");
    }

    /// One component at a known path, answering `run` with canned output.
    ///
    /// The binary these tests used to write and `chmod` proved nothing the
    /// canned answer does not: every branch below `run` reads an exit status
    /// and one string. What the stub added was a wait on the host getting a
    /// `#!/bin/sh` child to `main` inside [`PROBE_TIMEOUT`] — five seconds that
    /// measure machine load, and that a busy machine loses, reporting an
    /// installed component as absent. `git_cli::run_bounded_capped`'s own tests
    /// own the half that needs a real child.
    fn probed(
        id: &'static str,
        probe: Probe,
        run: &mut dyn FnMut(&str, &str, &[&str]) -> ProbeAnswer,
    ) -> ComponentStatus {
        probe_located_with(
            &Spec {
                id,
                label: id,
                need: ComponentNeed::HostResolved,
                purpose: "test",
                probe,
                presets: &["analysis"],
                managed: None,
                host_env: None,
                workbench_schema: None,
            },
            Some(format!("/nowhere/{id}")),
            run,
        )
    }

    /// The distinction this module exists to keep: a component that runs but
    /// cannot report a version is installed, and must not be shown the same as
    /// one that is absent.
    #[test]
    fn a_component_without_a_version_flag_is_still_installed() {
        let mut asked = Vec::new();
        let status = probed(
            "fake-dcstore",
            Probe::JsonHandshake(&[]),
            &mut |_, _, args| {
                asked.push(args.join(" "));
                Ok((
                    false,
                    "{\"ok\":false,\"error\":\"--db is required\"}\n".into(),
                ))
            },
        );
        assert_eq!(
            asked,
            vec![String::new()],
            "the handshake probe must run the component with its own arguments and nothing else"
        );
        assert!(status.installed, "{status:?}");
        assert!(
            matches!(status.version, VersionReading::NotExposed { .. }),
            "{:?}",
            status.version
        );
        assert!(status.reason.is_none());
    }

    /// A probe that could not run at all is the one case that must never read
    /// as an answer — not as a versionless install, and not as an impostor.
    #[test]
    fn a_probe_that_could_not_run_reports_why_and_claims_nothing() {
        let status = probed(
            "dcstore",
            Probe::VersionWithFallback(&[]),
            &mut |_, _, _| Err("dcstore timed out after 5s".into()),
        );
        assert!(
            !status.installed,
            "a component that never answered is not installed: {status:?}"
        );
        assert!(
            status
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("timed out"),
            "the reason must carry what went wrong: {:?}",
            status.reason
        );
        assert!(
            status.path.is_some(),
            "the path it tried is still worth reporting"
        );
    }

    /// A file with the right name that does not behave like the component is
    /// not the component.
    #[test]
    #[cfg(unix)]
    fn an_impostor_binary_is_not_reported_as_installed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().expect("tempdir");
        let bin = dir.path().join("not-really-dcgrep");
        std::fs::write(&bin, "#!/bin/sh\necho 'command not found'\n").expect("write");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let status = probe_located(
            &Spec {
                id: "not-really-dcgrep",
                label: "fake",
                need: ComponentNeed::HostResolved,
                purpose: "test",
                probe: Probe::JsonHandshake(&["health"]),
                presets: &["analysis"],
                managed: None,
                host_env: None,
                workbench_schema: None,
            },
            Some(bin.to_string_lossy().into_owned()),
        );
        assert!(status.path.is_some(), "the fixture must actually have run");
        assert!(!status.installed, "{status:?}");
        assert!(status.reason.is_some());
    }

    /// When a component returns the universal JSON payload with matching identity
    /// and version, its version is reported directly as the clean version number.
    #[test]
    fn universal_version_and_id_are_reported_for_analysis_components() {
        let cases = [
            ("dcstore", "{\"ok\":true,\"id\":\"dcstore\",\"component\":\"dc-store\",\"version\":\"0.2.3\"}\n"),
            ("dcverify", "{\"ok\":true,\"id\":\"dcverify\",\"component\":\"dc-verify\",\"version\":\"0.2.3\"}\n"),
            ("dcgrep", "{\"ok\":true,\"id\":\"dcgrep\",\"component\":\"dc-grep\",\"version\":\"0.2.3\"}\n"),
        ];

        for (id, payload) in cases {
            let status = probed(id, Probe::VersionWithFallback(&[]), &mut |_, _, args| {
                assert_eq!(args, ["--version"], "the version probe asks for --version");
                Ok((true, payload.to_string()))
            });
            assert!(
                status.installed,
                "expected {id} to be installed: {status:?}"
            );
            assert_eq!(
                status.version,
                VersionReading::Reported {
                    version: "0.2.3".into()
                },
                "expected clean universal version number for {id}"
            );
            assert!(status.reason.is_none());
        }
    }

    /// An impostor binary returning JSON with an unexpected id is rejected.
    #[test]
    fn an_impostor_with_wrong_id_is_rejected_as_unavailable() {
        let status = probed(
            "dcstore",
            Probe::VersionWithFallback(&[]),
            &mut |_, _, _| {
                Ok((
                true,
                "{\"ok\":true,\"id\":\"rogue_tool\",\"component\":\"rogue\",\"version\":\"1.0.0\"}\n"
                    .into(),
            ))
            },
        );

        assert!(
            !status.installed,
            "impostor must not be reported as installed: {status:?}"
        );
        assert!(
            matches!(status.version, VersionReading::Unavailable { .. }),
            "expected unavailable version reading: {:?}",
            status.version
        );
        assert!(
            status
                .reason
                .as_deref()
                .unwrap_or("")
                .contains("reported identity"),
            "expected reason to mention reported identity mismatch: {:?}",
            status.reason
        );
    }

    /// When `--version` fails on an older binary, fallback handshake executes and
    /// reports `NotExposed` instead of failing if the handshake succeeds.
    #[test]
    fn legacy_binary_falling_back_to_handshake_reports_not_exposed() {
        let mut asked = Vec::new();
        let status = probed(
            "dcstore",
            Probe::VersionWithFallback(&[]),
            &mut |_, _, args| {
                asked.push(args.join(" "));
                if args == ["--version"] {
                    return Ok((false, "unknown flag --version\n".into()));
                }
                Ok((
                    false,
                    "{\"ok\":false,\"error\":\"--db is required\"}\n".into(),
                ))
            },
        );

        assert_eq!(
            asked,
            vec!["--version".to_string(), String::new()],
            "a refused --version must be followed by the fallback handshake, in that order"
        );
        assert!(
            status.installed,
            "legacy binary must still be detected as installed: {status:?}"
        );
        assert!(
            matches!(status.version, VersionReading::NotExposed { .. }),
            "expected NotExposed version reading for legacy binary: {:?}",
            status.version
        );
        assert!(status.reason.is_none());
    }

    /// The host's override names the binary it runs, so it is the one
    /// probed: a path as given, a bare name looked up.
    #[test]
    fn a_host_override_is_the_binary_located() {
        assert_eq!(
            host_located("dcstore", Some(std::ffi::OsStr::new("/opt/dc/bin/dcstore"))).as_deref(),
            Some("/opt/dc/bin/dcstore")
        );
        assert_eq!(
            host_located(
                "dcstore",
                Some(std::ffi::OsStr::new("gitpulse-no-such-store-binary"))
            ),
            None,
            "a bare name the host cannot find either is not installed"
        );
        let store = catalogue()
            .into_iter()
            .find(|spec| spec.id == "dcstore")
            .unwrap();
        assert_eq!(store.host_env, Some("MANVI_STORE_BINARY"));
    }

    /// The catalogue's own spec for `id`, probed at a known path with canned
    /// answers: what the inventory says about the real component.
    fn probed_as_catalogued(
        id: &str,
        run: &mut dyn FnMut(&str, &str, &[&str]) -> ProbeAnswer,
    ) -> ComponentStatus {
        let spec = catalogue()
            .into_iter()
            .find(|spec| spec.id == id)
            .expect("catalogued");
        probe_located_with(&spec, Some(format!("/stale/bin/{id}")), run)
    }

    fn dcstore_answering(payload: &str) -> ComponentStatus {
        probed_as_catalogued("dcstore", &mut |_, _, args| {
            assert_eq!(args, ["--version"]);
            Ok((true, payload.to_string()))
        })
    }

    /// A `dcstore` that opens the workbench schema GitPulse's vendored store
    /// opens is installed, with its version.
    #[test]
    fn a_dcstore_reporting_the_vendored_workbench_schema_is_installed() {
        let status = dcstore_answering(&format!(
            "{{\"ok\":true,\"id\":\"dcstore\",\"component\":\"dc-store\",\"version\":\"0.2.4\",\"workbench_schema\":{}}}\n",
            crate::workbench::WORKBENCH_SCHEMA
        ));
        assert!(status.installed, "{status:?}");
        assert_eq!(
            status.version,
            VersionReading::Reported {
                version: "0.2.4".into()
            }
        );
        assert!(status.reason.is_none(), "{status:?}");
    }

    /// Manvi runs whichever `dcstore` it resolves against the profile. One
    /// that does not open GitPulse's workbench schema refuses the profile (or,
    /// newer, would migrate it past what GitPulse reads), so it is not an
    /// installed dcstore: it is one that needs updating, and says which.
    #[test]
    fn a_dcstore_on_another_workbench_schema_needs_updating() {
        let required = crate::workbench::WORKBENCH_SCHEMA;
        for (payload, says) in [
            (
                "{\"ok\":true,\"id\":\"dcstore\",\"component\":\"dc-store\",\"version\":\"0.2.4\"}\n".to_string(),
                "does not report".to_string(),
            ),
            (
                format!("{{\"ok\":true,\"id\":\"dcstore\",\"component\":\"dc-store\",\"version\":\"0.2.3\",\"workbench_schema\":{}}}\n", required - 1),
                format!("workbench schema {}", required - 1),
            ),
            (
                format!("{{\"ok\":true,\"id\":\"dcstore\",\"component\":\"dc-store\",\"version\":\"0.3.0\",\"workbench_schema\":{}}}\n", required + 1),
                format!("workbench schema {}", required + 1),
            ),
            (
                format!("{{\"ok\":true,\"id\":\"dcstore\",\"component\":\"dc-store\",\"version\":\"0.2.4\",\"workbench_schema\":\"{required}\"}}\n"),
                "does not report".to_string(),
            ),
        ] {
            let status = dcstore_answering(&payload);
            assert!(!status.installed, "{payload}: {status:?}");
            let reason = status.reason.as_deref().unwrap_or_default();
            assert!(reason.contains(&says), "{payload}: {reason}");
            assert!(
                reason.contains(&format!("schema {required}")) && reason.contains("update"),
                "the reason names what GitPulse needs and what to do: {reason}"
            );
            assert!(
                reason.contains("/stale/bin/dcstore"),
                "the reason names the binary Manvi would run: {reason}"
            );
            assert_eq!(status.path.as_deref(), Some("/stale/bin/dcstore"));
        }
    }

    /// A `dcstore` too old to answer `--version` is older still: it cannot
    /// say which workbench schema it opens, so it is not accepted either.
    #[test]
    fn a_dcstore_that_cannot_report_its_workbench_schema_needs_updating() {
        let status = probed_as_catalogued("dcstore", &mut |_, _, args| {
            if args == ["--version"] {
                return Ok((false, "unknown flag --version\n".into()));
            }
            Ok((
                false,
                "{\"ok\":false,\"error\":\"--db is required\"}\n".into(),
            ))
        });
        assert!(!status.installed, "{status:?}");
        assert!(
            status
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("does not report"),
            "{status:?}"
        );
        // A plain-text version line names no schema either.
        let status = probed_as_catalogued("dcstore", &mut |_, _, _| {
            Ok((true, "dcstore 0.2.1\n".into()))
        });
        assert!(!status.installed, "{status:?}");
    }

    /// Only the store is held to a workbench schema: the verifier and the
    /// searcher answering without one are as installed as before.
    #[test]
    fn only_dcstore_is_held_to_the_workbench_schema() {
        for id in ["dcverify", "dcgrep"] {
            let status = probed_as_catalogued(id, &mut |_, _, _| {
                Ok((
                    true,
                    format!("{{\"ok\":true,\"id\":\"{id}\",\"version\":\"0.2.4\"}}\n"),
                ))
            });
            assert!(status.installed, "{id}: {status:?}");
        }
    }

    /// A probe that did not run must never look like a probe that found
    /// nothing. Without a repository, doctor is absent *and* says why, and the
    /// flattened warning list stays empty rather than reading as "all clear".
    #[test]
    fn an_unchecked_installation_is_not_reported_as_healthy() {
        let report = suite_report(None);
        assert!(report.doctor.is_none());
        assert!(report.doctor_reason.is_some());
        assert!(report.warnings.is_empty());

        let untrusted = tempfile::TempDir::new().expect("tempdir");
        let report = suite_report(Some(untrusted.path().to_str().expect("utf8")));
        assert!(
            report.doctor_reason.is_some(),
            "an untrusted path must not silently become a clean bill of health"
        );
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn preset_counts_never_exceed_their_membership() {
        let status = suite_status();
        for preset in &status.presets {
            assert!(preset.present <= preset.total, "{preset:?}");
            assert_eq!(
                preset.total - preset.present,
                preset.missing.len(),
                "{preset:?}"
            );
        }
    }

    /// An optional component must never be what makes the suite incomplete —
    /// GitPulse does not start the Go host, so its absence is not a problem to
    /// report as one.
    #[test]
    fn optional_components_do_not_make_the_suite_incomplete() {
        let status = suite_status();
        let optional_missing = status
            .components
            .iter()
            .any(|c| !c.installed && matches!(c.need, ComponentNeed::Optional));
        let needed_missing = status
            .components
            .iter()
            .any(|c| !c.installed && !matches!(c.need, ComponentNeed::Optional));
        if optional_missing && !needed_missing {
            assert!(status.complete, "optional absence must not read as broken");
        }
    }

    /// If dcstore, dcverify, or dcgrep are installed on the host, verify that
    /// they report clean universal version numbers and validate component identity.
    #[test]
    fn host_installed_components_report_universal_version_when_present() {
        let status = suite_status();
        for id in &["dcstore", "dcverify", "dcgrep"] {
            if let Some(comp) = status.components.iter().find(|c| c.id == *id) {
                if comp.installed {
                    match &comp.version {
                        VersionReading::Reported { version } => {
                            assert!(!version.is_empty(), "version must not be empty for {id}");
                            assert!(
                                !version.contains('{'),
                                "version for {id} must be clean, not raw JSON: {version}"
                            );
                        }
                        VersionReading::NotExposed { .. } => {
                            // Valid fallback for older binaries
                        }
                        VersionReading::Unavailable { detail } => {
                            panic!("Installed component {id} should not be Unavailable: {detail}");
                        }
                    }
                }
            }
        }
    }
}
