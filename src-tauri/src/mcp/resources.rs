//! The `resources` capability: addressable, argument-free views of the control
//! plane.
//!
//! Tools answer parameterised questions; resources are documents a host can put
//! in a picker, cache by URI, and hand to a model without a tool call. The two
//! overlap in subject and not in shape, which is why they are separate modules
//! rather than one with a flag — except for the composed `status` document,
//! which had exactly one definition inline in the tool dispatch and now has
//! exactly one here that both call.
//!
//! Every repository-scoped facet is a *template*, not a listed resource: the
//! set returned by `resources/list` **MUST NOT** vary per connection
//! ([resources#capabilities](https://modelcontextprotocol.io/specification/2026-07-28/server/resources)),
//! and which repositories exist is a property of the machine, not of this
//! server. Listing them would make the catalog depend on the caller's disk.

use serde_json::{json, Value};

use super::uri::{self, Target, UriError};

/// Ceiling on one resource document, in bytes of serialized JSON.
///
/// stdio has no backpressure: a 200 MB `insights` payload for a monorepo is
/// written to the pipe in one `writeln!` and the client has to buffer all of
/// it. Truncation here is always *reported* — never a silently shortened
/// document that reads as complete.
pub const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;

/// A repository facet: one `gitpulse://<facet>{+repo_path}` template.
struct Facet {
    name: &'static str,
    title: &'static str,
    description: &'static str,
}

/// Advertised facets, in a stable order so the template list is cacheable.
const FACETS: &[Facet] = &[
    Facet {
        name: "insights",
        title: "Repository insights",
        description: "Worktrees, agent sessions, uncommitted changes, overlapping dirty files, ledger and code-graph availability. Facets fail independently, so a missed scan is reported rather than read as clean.",
    },
    Facet {
        name: "status",
        title: "Repository status",
        description: "Ledger status, code-graph availability, and the worktree list for one repository.",
    },
    Facet {
        name: "collisions",
        title: "Collision risk",
        description: "Files with uncommitted changes in more than one worktree. Unscanned worktrees are counted, never implied clean.",
    },
    Facet {
        name: "changes",
        title: "Active changes",
        description: "Working-tree file list for the repository's main worktree (path, staged, conflicted, churn), capped.",
    },
    Facet {
        name: "context",
        title: "Change context",
        description: "In-flight context: branch, dirty files, parked merge or rebase, bound task, and the collisions that involve this worktree.",
    },
    Facet {
        name: "ledger",
        title: "Ledger events",
        description: "Most recent durable ledger events (actor, tool, verdict, changes).",
    },
    Facet {
        name: "tasks",
        title: "Task view",
        description: "Task details, leases, and worktree bindings from dc-store.",
    },
    Facet {
        name: "codeintel",
        title: "Code graph status",
        description: "Whether the devmap code graph is present for this repository, and how current it is.",
    },
];

/// Server documents that need no repository. These are the whole of
/// `resources/list`.
const SERVER_DOCS: &[Facet] = &[
    Facet {
        name: "manifest",
        title: "Server manifest",
        description: "This server's identity, protocol version, advertised capabilities, and full tool catalog.",
    },
    Facet {
        name: "health",
        title: "Server health",
        description: "Runtime posture: agent-harness availability and gate posture, descriptor headroom, and build version.",
    },
];

pub fn capability() -> Value {
    // Neither `listChanged` nor `subscribe`: this server pushes nothing, and
    // advertising a notification it never sends would make a client wait for
    // one. The spec allows omitting both.
    json!({})
}

/// Concrete, listable resources — the server documents only.
pub fn list() -> Vec<Value> {
    SERVER_DOCS
        .iter()
        .map(|doc| {
            json!({
                "uri": format!("{}server/{}", uri::SCHEME, doc.name),
                "name": doc.name,
                "title": doc.title,
                "description": doc.description,
                "mimeType": "application/json",
            })
        })
        .collect()
}

/// Repository-scoped facets, as RFC 6570 templates.
pub fn templates() -> Vec<Value> {
    FACETS
        .iter()
        .map(|facet| {
            json!({
                "uriTemplate": format!("{}{}{{+repo_path}}", uri::SCHEME, facet.name),
                "name": facet.name,
                "title": facet.title,
                "description": facet.description,
                "mimeType": "application/json",
            })
        })
        .collect()
}

/// Facet names, for `completion/complete` and for the catalog contract test.
pub fn facet_names() -> Vec<&'static str> {
    FACETS.iter().map(|f| f.name).collect()
}

/// Why a read failed, kept distinct so the caller can pick the right JSON-RPC
/// code: a URI this server does not address is `-32602`, a backend that broke
/// is `-32603`. Collapsing them would tell a client to fix its URI when the
/// disk was the problem.
#[derive(Debug)]
pub enum ReadError {
    NotFound(String),
    Internal(String),
}

impl ReadError {
    pub fn message(&self) -> &str {
        match self {
            Self::NotFound(m) | Self::Internal(m) => m,
        }
    }
}

impl From<UriError> for ReadError {
    fn from(error: UriError) -> Self {
        Self::NotFound(error.to_string())
    }
}

/// Read one resource.
///
/// Returns the `contents` array for a `resources/read` result. It is never
/// empty on success: the spec forbids an empty `contents` for a resource that
/// does not exist, because a client cannot tell that from a resource that
/// exists and is blank — so a missing one is an error, not `[]`.
pub fn read(target_uri: &str) -> Result<Vec<Value>, ReadError> {
    let payload = match uri::parse(target_uri)? {
        Target::Server(name) => server_document(&name)?,
        Target::Repo { facet, path } => repo_facet(&facet, &path)?,
    };
    Ok(vec![text_content(target_uri, payload)])
}

fn text_content(target_uri: &str, payload: Value) -> Value {
    let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|error| {
        // Reachable if a backend ever produces a non-finite float, which
        // serde_json refuses to serialize. Saying so beats an empty document.
        json!({ "ok": false, "error": format!("result could not be serialized: {error}") })
            .to_string()
    });
    let (text, truncated) = if text.len() > MAX_DOCUMENT_BYTES {
        let mut cut = MAX_DOCUMENT_BYTES;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        (
            format!(
                "{}\n\n… truncated: document is {} bytes, capped at {}. \
                 Use the equivalent tool with a narrower argument to read it in full.",
                &text[..cut],
                text.len(),
                MAX_DOCUMENT_BYTES
            ),
            true,
        )
    } else {
        (text, false)
    };
    json!({
        "uri": target_uri,
        // Truncated JSON is no longer JSON, and labelling it as such would make
        // a client's parse failure look like a server bug rather than a cap.
        "mimeType": if truncated { "text/plain" } else { "application/json" },
        "text": text,
    })
}

fn server_document(name: &str) -> Result<Value, ReadError> {
    match name {
        "manifest" => Ok(json!({
            "name": super::SERVER_NAME,
            "version": super::server_version(),
            "storeSchemaVersion": crate::codeintel::SUPPORTED_STORE_SCHEMA,
            "protocolVersion": super::PROTOCOL_VERSION,
            "legacyVersions": super::LEGACY_VERSIONS,
            "readOnly": true,
            "capabilities": super::capabilities(),
            "tools": super::tools(),
            "resources": list(),
            "resourceTemplates": templates(),
            "prompts": super::prompts::list(),
        })),
        "health" => {
            let harness = crate::harness::HarnessStatus::probe();
            Ok(json!({
                "version": super::server_version(),
                "harness": {
                    "available": harness.available,
                    "binary": harness.binary,
                    "protocol": harness.protocol,
                    "posture": harness.posture,
                    "ops": harness.ops,
                    "error": harness.error,
                    "errorCode": harness.error_code,
                },
                // Reported rather than asserted: with no harness installed every
                // guarded action runs unchecked, and that has to be visible.
                "commandGate": if harness.available { "active" } else { "absent: mutating git actions run unguarded" },
                "openFileLimit": crate::limits::describe_open_file_limit(),
            }))
        }
        other => Err(ReadError::NotFound(format!(
            "no server document named {other:?}; known: [{}]",
            SERVER_DOCS
                .iter()
                .map(|d| d.name)
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// The composed repository status document.
///
/// The tool dispatch calls this too — it is the one shape that genuinely had
/// two would-be owners.
pub fn status_document(repo: &str) -> Result<Value, String> {
    const WORKTREE_CAP: usize = 64;
    // The read-only variant: the creating one opens the database (which makes
    // it) and runs the legacy consolidation (which migrates rows), neither of
    // which a tool annotated `readOnlyHint: true` may do to someone's repo.
    let ledger_status = crate::ledger::bindings::repository_status_readonly(repo)
        .map_err(|error| error.to_string())?;
    let codeintel_status = crate::codeintel::status(repo);
    let worktrees = match crate::engine::worktree::list_worktrees(repo) {
        Ok(list) => {
            let total = list.len();
            let items: Vec<Value> = list
                .into_iter()
                .take(WORKTREE_CAP)
                .map(|w| {
                    json!({
                        "path": w.path,
                        "name": w.name,
                        "branch": w.branch,
                        "is_main": w.is_main,
                        "is_bare": w.is_bare,
                        "dirty_files": w.dirty_files,
                    })
                })
                .collect();
            json!({
                "ok": true,
                "count": total,
                "truncated": total > WORKTREE_CAP,
                "items": items,
            })
        }
        // A scan that could not run is reported as such. An empty `items` with
        // `ok: true` would read as "this repository has no worktrees".
        Err(error) => json!({
            "ok": false,
            "error": error,
            "count": 0,
            "truncated": false,
            "items": [],
        }),
    };
    Ok(json!({
        "repo_path": repo,
        "ledger": ledger_status,
        "codeintel": codeintel_status,
        "worktrees": worktrees,
    }))
}

fn repo_facet(facet: &str, repo: &str) -> Result<Value, ReadError> {
    match facet {
        "insights" => Ok(json!(crate::insights::snapshot(repo))),
        "status" => status_document(repo).map_err(ReadError::Internal),
        "collisions" => Ok(json!(crate::insights::collision_risk(repo))),
        "changes" => Ok(json!(crate::insights::active_changes(repo, None, None))),
        "context" => Ok(json!(crate::insights::change_context(repo, None))),
        "ledger" => {
            let address = crate::ledger::bindings::repository_address(repo)
                .map_err(|error| ReadError::Internal(error.to_string()))?;
            match crate::ledger::tail_readonly(&address.anchor, 0, super::LEDGER_DEFAULT_LIMIT)
                .map_err(|error| ReadError::Internal(error.to_string()))?
            {
                Some(events) => Ok(json!({
                    "ok": true, "returned": events.len(), "events": events
                })),
                // No ledger is not an empty ledger, and must not read as one.
                None => Ok(json!({
                    "ok": false,
                    "error": "no ledger in this repository yet; nothing has been recorded",
                    "returned": 0,
                    "events": [],
                })),
            }
        }
        "tasks" => {
            let address = crate::ledger::bindings::repository_address(repo)
                .map_err(|error| ReadError::Internal(error.to_string()))?;
            Ok(json!(crate::tasks::view(&address.anchor)))
        }
        "codeintel" => Ok(json!(crate::codeintel::status(repo))),
        other => Err(ReadError::NotFound(format!(
            "no facet named {other:?}; known: [{}]",
            facet_names().join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `"name" =>` match arm in this file.
    fn dispatch_arms() -> std::collections::BTreeSet<String> {
        include_str!("resources.rs")
            .lines()
            .filter_map(|line| {
                let (name, tail) = line.trim().strip_prefix('"')?.split_once('"')?;
                tail.trim_start()
                    .starts_with("=>")
                    .then(|| name.to_string())
            })
            .collect()
    }

    #[test]
    fn every_advertised_template_has_a_read_arm_and_the_reverse() {
        // Derived, not hand-listed: a facet added to FACETS without a match arm
        // — or an arm left behind after a facet was withdrawn — fails here
        // rather than at a client's first read.
        let advertised: std::collections::BTreeSet<String> =
            facet_names().into_iter().map(str::to_string).collect();
        let arms = dispatch_arms();
        assert!(
            advertised.len() >= 8,
            "scan found only {}",
            advertised.len()
        );
        for facet in &advertised {
            assert!(
                arms.contains(facet),
                "{facet} is advertised with no read arm"
            );
        }
        let server: std::collections::BTreeSet<String> =
            SERVER_DOCS.iter().map(|d| d.name.to_string()).collect();
        for arm in &arms {
            assert!(
                advertised.contains(arm) || server.contains(arm),
                "{arm} is dispatched but advertised nowhere"
            );
        }
    }

    #[test]
    fn every_listed_server_document_has_a_read_arm_and_the_reverse() {
        let arms = dispatch_arms();
        for doc in SERVER_DOCS {
            assert!(
                arms.contains(doc.name),
                "{} is listed with no read arm",
                doc.name
            );
        }
    }

    #[test]
    fn listed_resources_are_addressable_by_the_uri_they_advertise() {
        for resource in list() {
            let target = resource["uri"].as_str().unwrap();
            assert!(
                matches!(uri::parse(target), Ok(Target::Server(_))),
                "{target} does not parse"
            );
            assert!(read(target).is_ok(), "{target} does not read");
        }
    }

    #[test]
    fn every_template_expands_to_a_uri_that_parses_back_to_its_facet() {
        for template in templates() {
            let raw = template["uriTemplate"].as_str().unwrap();
            let name = template["name"].as_str().unwrap();
            // The only variable is `{+repo_path}`; expanding it by hand is what
            // a client does, so it is what the test does.
            let expanded = raw.replace("{+repo_path}", &uri::encode_path("/tmp/example repo"));
            match uri::parse(&expanded).expect("expands to a valid URI") {
                Target::Repo { facet, path } => {
                    assert_eq!(facet, name);
                    assert_eq!(path, "/tmp/example repo");
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn an_unknown_facet_names_the_ones_that_exist() {
        let error = read("gitpulse://not_a_facet/tmp/x").expect_err("unknown facet");
        assert!(matches!(error, ReadError::NotFound(_)));
        assert!(error.message().contains("insights"), "{}", error.message());
    }

    #[test]
    fn a_traversal_uri_is_refused_before_any_backend_runs() {
        let error = read("gitpulse://insights/tmp/../../etc").expect_err("traversal");
        assert!(matches!(error, ReadError::NotFound(_)));
        assert!(error.message().contains(".."), "{}", error.message());
    }

    #[test]
    fn a_missing_repository_is_an_error_never_an_empty_contents_array() {
        // The spec forbids answering a non-existent resource with `[]`: a client
        // cannot tell that from a resource that exists and is empty.
        let error = read("gitpulse://status/no/such/repo").expect_err("missing repo");
        assert!(matches!(error, ReadError::Internal(_)));
        assert!(!error.message().is_empty());
    }

    #[test]
    fn an_oversized_document_is_labelled_truncated_and_stops_claiming_to_be_json() {
        let big = json!({ "blob": "x".repeat(MAX_DOCUMENT_BYTES + 1024) });
        let content = text_content("gitpulse://server/manifest", big);
        let text = content["text"].as_str().unwrap();
        assert!(text.contains("… truncated"), "truncation is not announced");
        assert_eq!(content["mimeType"], "text/plain");
        assert!(text.len() < MAX_DOCUMENT_BYTES + 4096);
    }

    #[test]
    fn a_document_under_the_cap_is_untouched_json() {
        let content = text_content("gitpulse://server/health", json!({ "a": 1 }));
        assert_eq!(content["mimeType"], "application/json");
        let text = content["text"].as_str().unwrap();
        assert!(!text.contains("truncated"));
        assert_eq!(
            serde_json::from_str::<Value>(text).unwrap(),
            json!({ "a": 1 })
        );
    }

    #[test]
    fn the_manifest_document_reports_the_real_catalog_not_a_copy_of_it() {
        let contents = read("gitpulse://server/manifest").expect("manifest reads");
        let text = contents[0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).expect("manifest is JSON");
        assert_eq!(parsed["name"], super::super::SERVER_NAME);
        assert_eq!(parsed["protocolVersion"], super::super::PROTOCOL_VERSION);
        assert_eq!(
            parsed["tools"].as_array().unwrap().len(),
            super::super::tools().len()
        );
        assert_eq!(parsed["readOnly"], true);
        assert_eq!(
            parsed["storeSchemaVersion"],
            crate::codeintel::SUPPORTED_STORE_SCHEMA
        );
    }

    #[test]
    fn the_health_document_says_so_when_the_command_gate_is_absent() {
        let contents = read("gitpulse://server/health").expect("health reads");
        let text = contents[0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).expect("health is JSON");
        let gate = parsed["commandGate"].as_str().unwrap();
        let available = parsed["harness"]["available"].as_bool().unwrap();
        // Whichever way this machine is configured, the two must agree — the
        // failure this guards is a gate reported as active because the probe
        // itself failed.
        assert_eq!(available, gate == "active", "{gate}");
    }

    #[test]
    fn server_documents_and_facets_do_not_share_a_name() {
        // `gitpulse://server/...` is parsed by the `server` authority, so a facet
        // called `server` would be unreachable.
        assert!(!facet_names().contains(&"server"));
    }
}
