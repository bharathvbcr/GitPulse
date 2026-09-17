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
        },
        Spec {
            id: "manvi",
            label: "Manvi",
            need: ComponentNeed::Required,
            purpose: "Hosts policy, the profile workbench and managed agent runs.",
            probe: Probe::VersionFlag,
            presets: &["all"],
            managed: Some(ExternalTool::Manvi),
        },
        Spec {
            id: "dcstore",
            label: "dcstore",
            need: ComponentNeed::HostResolved,
            purpose: "Manvi opens the profile workbench through it; without it managed runs and enhancements fail.",
            probe: Probe::VersionWithFallback(&[]),
            presets: &["analysis", "all"],
            managed: None,
        },
        Spec {
            id: "dcverify",
            label: "dcverify",
            need: ComponentNeed::HostResolved,
            purpose: "Manvi's verification gate. GitPulse links the same library, so its own reads work without the binary.",
            probe: Probe::VersionWithFallback(&[]),
            presets: &["analysis", "all"],
            managed: None,
        },
        Spec {
            id: "dcgrep",
            label: "dcgrep",
            need: ComponentNeed::HostResolved,
            purpose: "Manvi's repository search. GitPulse does not spawn it directly.",
            probe: Probe::VersionWithFallback(&["health"]),
            presets: &["analysis", "all"],
            managed: None,
        },
        Spec {
            id: "devcouncil",
            label: "DevCouncil host",
            need: ComponentNeed::Optional,
            purpose: "The Go orchestrator. GitPulse talks to Manvi instead and never starts this.",
            probe: Probe::VersionFlag,
            presets: &["all"],
            managed: None,
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
    git_cli::find_external_tool(spec.id)
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

/// The probe itself, with lookup already done.
///
/// Split out so a test can drive a real executable at a known path: passing a
/// path through `Spec::id` instead would make every fixture resolve to "not
/// found", and a test asserting "not installed" would pass without ever
/// running the binary it claims to reject.
fn probe_located(spec: &Spec<'_>, located: Option<String>) -> ComponentStatus {
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

    match spec.probe {
        Probe::VersionFlag | Probe::VersionWithFallback(_) => {
            let fallback_args = match spec.probe {
                Probe::VersionWithFallback(args) => Some(args),
                _ => None,
            };

            match run_probe(&path, spec.id, &["--version"]) {
                Ok((true, text)) => {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text.trim()) {
                        match evaluate_json_response(&value, spec.id) {
                            JsonProbeOutcome::Reported { version } => {
                                base(true, Some(path), VersionReading::Reported { version }, None)
                            }
                            JsonProbeOutcome::NotExposed { detail } => {
                                base(true, Some(path), VersionReading::NotExposed { detail }, None)
                            }
                            JsonProbeOutcome::IdentityMismatch { reported } => base(
                                false,
                                Some(path),
                                VersionReading::Unavailable {
                                    detail: format!("reported unexpected component identity {reported:?}"),
                                },
                                Some(format!(
                                    "`{}` did not answer like a DevCouncil component: reported identity {reported:?}",
                                    spec.id
                                )),
                            ),
                            JsonProbeOutcome::Malformed { detail } => base(
                                false,
                                Some(path),
                                VersionReading::Unavailable {
                                    detail: detail.clone(),
                                },
                                Some(format!(
                                    "`{}` did not answer like a DevCouncil component: {detail}",
                                    spec.id
                                )),
                            ),
                        }
                    } else {
                        let reading = match super::version_line(&text, spec.id) {
                            Some(version) => VersionReading::Reported { version },
                            None => VersionReading::NotExposed {
                                detail: "`--version` printed nothing this reader recognized".into(),
                            },
                        };
                        base(true, Some(path), reading, None)
                    }
                }
                Ok((false, text)) => {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text.trim()) {
                        if value.is_object() {
                            match evaluate_json_response(&value, spec.id) {
                                JsonProbeOutcome::Reported { version } => {
                                    return base(
                                        true,
                                        Some(path),
                                        VersionReading::Reported { version },
                                        None,
                                    );
                                }
                                JsonProbeOutcome::NotExposed { detail } => {
                                    return base(
                                        true,
                                        Some(path),
                                        VersionReading::NotExposed { detail },
                                        None,
                                    );
                                }
                                JsonProbeOutcome::IdentityMismatch { reported } => {
                                    return base(
                                        false,
                                        Some(path),
                                        VersionReading::Unavailable {
                                            detail: format!("reported unexpected component identity {reported:?}"),
                                        },
                                        Some(format!(
                                            "`{}` did not answer like a DevCouncil component: reported identity {reported:?}",
                                            spec.id
                                        )),
                                    );
                                }
                                _ => {}
                            }
                        }
                    }

                    if let Some(args) = fallback_args {
                        if let Ok((_, fallback_text)) = run_probe(&path, spec.id, args) {
                            if let Ok(value) =
                                serde_json::from_str::<serde_json::Value>(fallback_text.trim())
                            {
                                if value.is_object() {
                                    return match evaluate_json_response(&value, spec.id) {
                                        JsonProbeOutcome::Reported { version } => {
                                            base(true, Some(path), VersionReading::Reported { version }, None)
                                        }
                                        JsonProbeOutcome::NotExposed { detail } => {
                                            base(true, Some(path), VersionReading::NotExposed { detail }, None)
                                        }
                                        JsonProbeOutcome::IdentityMismatch { reported } => base(
                                            false,
                                            Some(path),
                                            VersionReading::Unavailable {
                                                detail: format!("reported unexpected component identity {reported:?}"),
                                            },
                                            Some(format!(
                                                "`{}` did not answer like a DevCouncil component: reported identity {reported:?}",
                                                spec.id
                                            )),
                                        ),
                                        JsonProbeOutcome::Malformed { detail } => base(
                                            false,
                                            Some(path),
                                            VersionReading::Unavailable {
                                                detail: detail.clone(),
                                            },
                                            Some(format!(
                                                "`{}` did not answer like a DevCouncil component: {detail}",
                                                spec.id
                                            )),
                                        ),
                                    };
                                }
                            }
                        }
                    }

                    let detail = text.trim().chars().take(200).collect::<String>();
                    base(
                        false,
                        Some(path),
                        VersionReading::Unavailable {
                            detail: detail.clone(),
                        },
                        Some(format!("`{} --version` failed: {detail}", spec.id)),
                    )
                }
                Err(detail) => {
                    if let Some(args) = fallback_args {
                        if let Ok((_, fallback_text)) = run_probe(&path, spec.id, args) {
                            if let Ok(value) =
                                serde_json::from_str::<serde_json::Value>(fallback_text.trim())
                            {
                                if value.is_object() {
                                    return match evaluate_json_response(&value, spec.id) {
                                        JsonProbeOutcome::Reported { version } => {
                                            base(true, Some(path), VersionReading::Reported { version }, None)
                                        }
                                        JsonProbeOutcome::NotExposed { detail } => {
                                            base(true, Some(path), VersionReading::NotExposed { detail }, None)
                                        }
                                        JsonProbeOutcome::IdentityMismatch { reported } => base(
                                            false,
                                            Some(path),
                                            VersionReading::Unavailable {
                                                detail: format!("reported unexpected component identity {reported:?}"),
                                            },
                                            Some(format!(
                                                "`{}` did not answer like a DevCouncil component: reported identity {reported:?}",
                                                spec.id
                                            )),
                                        ),
                                        JsonProbeOutcome::Malformed { detail } => base(
                                            false,
                                            Some(path),
                                            VersionReading::Unavailable {
                                                detail: detail.clone(),
                                            },
                                            Some(format!(
                                                "`{}` did not answer like a DevCouncil component: {detail}",
                                                spec.id
                                            )),
                                        ),
                                    };
                                }
                            }
                        }
                    }

                    base(
                        false,
                        Some(path),
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
        Probe::JsonHandshake(args) => match run_probe(&path, spec.id, args) {
            Ok((_, text)) => {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(text.trim()) {
                    if value.is_object() {
                        match evaluate_json_response(&value, spec.id) {
                            JsonProbeOutcome::Reported { version } => {
                                base(true, Some(path), VersionReading::Reported { version }, None)
                            }
                            JsonProbeOutcome::NotExposed { detail } => {
                                base(true, Some(path), VersionReading::NotExposed { detail }, None)
                            }
                            JsonProbeOutcome::IdentityMismatch { reported } => base(
                                false,
                                Some(path),
                                VersionReading::Unavailable {
                                    detail: format!("reported unexpected component identity {reported:?}"),
                                },
                                Some(format!(
                                    "`{}` did not answer like a DevCouncil component: reported identity {reported:?}",
                                    spec.id
                                )),
                            ),
                            JsonProbeOutcome::Malformed { detail } => base(
                                false,
                                Some(path),
                                VersionReading::Unavailable {
                                    detail: detail.clone(),
                                },
                                Some(format!(
                                    "`{}` did not answer like a DevCouncil component: {detail}",
                                    spec.id
                                )),
                            ),
                        }
                    } else {
                        let detail = text.trim().chars().take(200).collect::<String>();
                        base(
                            false,
                            Some(path),
                            VersionReading::Unavailable {
                                detail: detail.clone(),
                            },
                            Some(format!(
                                "`{}` did not answer like a DevCouncil component: {detail}",
                                spec.id
                            )),
                        )
                    }
                } else {
                    let detail = text.trim().chars().take(200).collect::<String>();
                    base(
                        false,
                        Some(path),
                        VersionReading::Unavailable {
                            detail: detail.clone(),
                        },
                        Some(format!(
                            "`{}` did not answer like a DevCouncil component: {detail}",
                            spec.id
                        )),
                    )
                }
            }
            Err(detail) => base(
                false,
                Some(path),
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
        };
        let status = probe(&spec);
        assert!(!status.installed);
        assert!(status.path.is_none());
        assert!(matches!(status.version, VersionReading::Unavailable { .. }));
        assert!(status.reason.is_some(), "a missing tool must say so");
    }

    /// The distinction this module exists to keep: a component that runs but
    /// cannot report a version is installed, and must not be shown the same as
    /// one that is absent.
    #[test]
    fn a_component_without_a_version_flag_is_still_installed() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let bin = dir.path().join("fake-dcstore");
        std::fs::write(
            &bin,
            "#!/bin/sh\nprintf '%s\\n' '{\"ok\":false,\"error\":\"--db is required\"}'\n",
        )
        .expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");

            let status = probe_located(
                &Spec {
                    id: "fake-dcstore",
                    label: "fake",
                    need: ComponentNeed::HostResolved,
                    purpose: "test",
                    probe: Probe::JsonHandshake(&[]),
                    presets: &["analysis"],
                    managed: None,
                },
                Some(bin.to_string_lossy().into_owned()),
            );
            assert!(status.installed, "{status:?}");
            assert!(
                matches!(status.version, VersionReading::NotExposed { .. }),
                "{:?}",
                status.version
            );
            assert!(status.reason.is_none());
        }
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
    #[cfg(unix)]
    fn universal_version_and_id_are_reported_for_analysis_components() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().expect("tempdir");

        let cases = [
            ("dcstore", "{\"ok\":true,\"id\":\"dcstore\",\"component\":\"dc-store\",\"version\":\"0.2.3\"}\n"),
            ("dcverify", "{\"ok\":true,\"id\":\"dcverify\",\"component\":\"dc-verify\",\"version\":\"0.2.3\"}\n"),
            ("dcgrep", "{\"ok\":true,\"id\":\"dcgrep\",\"component\":\"dc-grep\",\"version\":\"0.2.3\"}\n"),
        ];

        for (id, payload) in cases {
            let bin = dir.path().join(id);
            std::fs::write(&bin, format!("#!/bin/sh\nprintf '%s' '{payload}'\n")).expect("write");
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");

            let spec = Spec {
                id,
                label: id,
                need: ComponentNeed::HostResolved,
                purpose: "test",
                probe: Probe::VersionWithFallback(&[]),
                presets: &["analysis"],
                managed: None,
            };

            let status = probe_located(&spec, Some(bin.to_string_lossy().into_owned()));
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
    #[cfg(unix)]
    fn an_impostor_with_wrong_id_is_rejected_as_unavailable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().expect("tempdir");
        let bin = dir.path().join("fake-dcstore-impostor");
        std::fs::write(
            &bin,
            "#!/bin/sh\nprintf '%s\\n' '{\"ok\":true,\"id\":\"rogue_tool\",\"component\":\"rogue\",\"version\":\"1.0.0\"}'\n",
        )
        .expect("write");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let status = probe_located(
            &Spec {
                id: "dcstore",
                label: "dcstore",
                need: ComponentNeed::HostResolved,
                purpose: "test",
                probe: Probe::VersionWithFallback(&[]),
                presets: &["analysis"],
                managed: None,
            },
            Some(bin.to_string_lossy().into_owned()),
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
    #[cfg(unix)]
    fn legacy_binary_falling_back_to_handshake_reports_not_exposed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().expect("tempdir");
        let bin = dir.path().join("legacy-dcstore");
        std::fs::write(
            &bin,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'unknown flag --version' >&2; exit 2; fi\nprintf '%s\\n' '{\"ok\":false,\"error\":\"--db is required\"}'\n",
        )
        .expect("write");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let status = probe_located(
            &Spec {
                id: "dcstore",
                label: "dcstore",
                need: ComponentNeed::HostResolved,
                purpose: "test",
                probe: Probe::VersionWithFallback(&[]),
                presets: &["analysis"],
                managed: None,
            },
            Some(bin.to_string_lossy().into_owned()),
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
