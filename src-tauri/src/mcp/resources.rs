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

use super::budget;
use super::uri::{self, Target, UriError};

/// Ceiling on one resource document, in bytes of the text as it is carried on
/// the wire: after JSON escaping, which grows a quote or a control character
/// two to six times.
///
/// stdio has no backpressure: a 200 MB `insights` payload for a monorepo is
/// written to the pipe in one `writeln!` and the client has to buffer all of
/// it. Truncation here is always *reported* — never a silently shortened
/// document that reads as complete.
///
/// Below the frame by a quarter megabyte, which is what the response around the
/// document (the envelope, the `uri`, the notice) may take. A cap equal to the
/// frame could never fit it: any document near the cap overflowed, and the
/// transport could only answer that with an error.
pub const MAX_DOCUMENT_BYTES: usize = super::MAX_FRAME_BYTES - 256 * 1024;

/// Room the notice appended to a cut document may take on the wire.
const NOTICE_RESERVE_BYTES: usize = 512;

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
        description: "Worktrees, agent sessions (running sessions per worktree path), uncommitted changes, overlapping dirty files, ledger and code-graph availability. Facets fail independently, so a missed scan is reported rather than read as clean.",
    },
    Facet {
        name: "status",
        title: "Repository status",
        description: "Ledger status, code-graph availability, and the worktree list for one repository.",
    },
    Facet {
        name: "collisions",
        title: "Collision risk",
        description: "Files with uncommitted changes in more than one worktree, and dirty files in any worktree two or more live agent sessions share. Unscanned worktrees are counted, never implied clean.",
    },
    Facet {
        name: "elsewhere",
        title: "Other worktrees' dirty files",
        description: "The uncommitted files of every other worktree, for a worktree that is itself clean. It cannot know which file you will edit; ask gitpulse_collision_risk with a path for that. A worktree with scanned false was not read.",
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
    // A character costs at most six bytes escaped, so a document under a sixth
    // of the cap cannot overflow it and is not measured (the manifest is read
    // on every `resources/read`, and counting its characters showed in the
    // bench).
    let (text, truncated) = if text.len() > MAX_DOCUMENT_BYTES / 6
        && budget::escaped_len(&text) - 2 > MAX_DOCUMENT_BYTES
    {
        // Cut where the *escaped* prefix fits, leaving room for the notice.
        let prefix = budget::escaped_prefix(&text, MAX_DOCUMENT_BYTES - NOTICE_RESERVE_BYTES);
        (
            format!(
                "{prefix}\n\n… truncated: document is {} bytes, capped at {} bytes as carried \
                 on the wire. Use the equivalent tool with a narrower argument to read it \
                 in full.",
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

/// devmap-extract declares its schema as a string (it forms the payload
/// identity `{ANALYZER_VERSION}:extract-v{N}`); the manifest reports it as the
/// number it names. A constant that is not a whole number reports `null`, which
/// the parity check reads as "did not say" rather than a match.
fn extraction_schema_version() -> Option<u64> {
    devmap_extract::cache::EXTRACTION_SCHEMA_VERSION
        .parse()
        .ok()
}

fn server_document(name: &str) -> Result<Value, ReadError> {
    match name {
        "manifest" => Ok(json!({
            "name": super::SERVER_NAME,
            "version": super::server_version(),
            "storeSchemaVersion": crate::codeintel::SUPPORTED_STORE_SCHEMA,
            // The two dc-store schemas this build embeds, so a cross-app check
            // can ask an installed binary rather than trust the source it
            // might not have been built from (scripts/check-schema-parity.mjs).
            "dcStoreSchemaVersion": dc_store::SCHEMA_VERSION,
            "workbenchSchemaVersion": dc_store::workbench::WORKBENCH_SCHEMA,
            "extractionSchemaVersion": extraction_schema_version(),
            "protocolVersion": super::PROTOCOL_VERSION,
            "legacyVersions": super::LEGACY_VERSIONS,
            // Derived from the tool annotations, so it cannot claim "read-only"
            // while a tool writes. It said `true` after the task-board tools
            // began writing the GitPulse task profile.
            "readOnly": super::tools().iter().all(|tool| tool["annotations"]["readOnlyHint"] == true),
            // True of `gitpulse_merge_conflicts` and `gitpulse_landed` too: a
            // merge-tree writes its objects to a quarantined directory that is
            // deleted afterwards, and the object-store tests hold that.
            "gitWrites": false,
            "writes": super::tools().iter()
                .filter(|tool| tool["annotations"]["readOnlyHint"] != true)
                .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
                .collect::<Vec<_>>(),
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

/// One page of the ledger from `cursor`, shared by `gitpulse_ledger_events` and
/// the `ledger` resource so the two cannot drift.
///
/// `tail` pages *forward* from a cursor — deliberately, so a consumer holding
/// the last id it saw gets the same answer whether it has been listening for
/// an hour or just opened. `returned` says how many came back, `truncated` is
/// true only when more events really follow (an exactly-full final page is not
/// truncated), and `next_cursor` is what to pass to get them.
///
/// A page is bounded in bytes as well as rows: each row is cut field by field
/// ([`crate::ledger::LedgerEvent::bounded`]) and the page ends at the last row
/// that fits [`crate::ledger::PAGE_MAX_BYTES`], so a thousand heavy rows come
/// back as several pages whose `next_cursor` continues without a gap.
pub fn ledger_page(anchor: &str, cursor: i64, limit: u32) -> Result<Value, String> {
    ledger_query_page(
        anchor,
        &crate::ledger::EventQuery {
            after: cursor,
            limit,
            ..crate::ledger::EventQuery::default()
        },
        None,
    )
}

/// What every ledger answer says about how current and how whole it is,
/// merged into the page: the [`crate::ledger::Freshness`] fields (`as_of`,
/// `last_catch_up`, `stale`, the last catch-up's counts including
/// `skipped_lines`) and the legacy-unconsolidated caveat, which is the same
/// text `status_readonly` carries (`caveat`, with its `caveat_code`; both
/// empty when there is none). A page with rows and no such block reads as
/// "this is everything, as of now", which nothing here can promise.
fn with_freshness(mut page: Value, anchor: &str, caveat: Option<&str>) -> Value {
    // The same status the status and insights facets serialize, minus the two
    // fields the page already spends on its own `error`.
    let view = json!(crate::ledger::status_readonly(anchor));
    if let (Some(page), Some(fields)) = (page.as_object_mut(), view.as_object()) {
        for (key, value) in fields {
            if key != "error" && key != "error_code" {
                page.insert(key.clone(), value.clone());
            }
        }
        page.insert("caveat".into(), json!(caveat.unwrap_or_default()));
        page.insert(
            "caveat_code".into(),
            json!(if caveat.is_some() {
                crate::ledger::bindings::LEGACY_UNCONSOLIDATED_CODE
            } else {
                ""
            }),
        );
    }
    page
}

/// [`ledger_page`] for a filtered, ordered query: the same bounds and the same
/// `truncated` / `next_cursor` contract, in the query's own order. In newest
/// first order `next_cursor` is the lowest id returned, to be passed back as
/// the next query's `before`.
pub fn ledger_query_page(
    anchor: &str,
    query: &crate::ledger::EventQuery,
    caveat: Option<&str>,
) -> Result<Value, String> {
    match crate::ledger::query_readonly_page(anchor, query, crate::ledger::PAGE_MAX_BYTES)
        .map_err(|e| e.to_string())?
    {
        Some(page) => {
            let next_cursor = page.events.last().map(|event| event.id);
            // The read took one row past the limit to settle this, so a full
            // final page is not truncated, and a page cut for its size always
            // has more behind it.
            let truncated = page.byte_capped || page.more;
            // Every row says whether its verdict was cut: `verdict_truncated`
            // is null for a whole one. The verdict has no room for a marker,
            // so the record is the row's own optional field.
            let events: Vec<Value> = page
                .events
                .iter()
                .map(|event| {
                    let mut row = json!(event);
                    row["verdict_truncated"] = page
                        .verdict_cuts
                        .get(&event.id)
                        .map_or(Value::Null, |cut| json!(cut));
                    row
                })
                .collect();
            Ok(with_freshness(
                json!({
                    "ok": true,
                    "returned": events.len(),
                    "truncated": truncated,
                    "next_cursor": next_cursor,
                    "events": events,
                }),
                anchor,
                caveat,
            ))
        }
        // No ledger is not an empty ledger, and must not read as one.
        None => Ok(with_freshness(
            json!({
                "ok": false,
                "error": "no ledger in this repository yet; nothing has been recorded",
                "returned": 0,
                "truncated": false,
                "events": [],
            }),
            anchor,
            caveat,
        )),
    }
}

/// The insights snapshot as MCP reports it: the in-process snapshot, which the
/// hooks consume as it is. Its `ledger` facet is already the honest status
/// (never `recording`, `dropped` scoped, with freshness).
pub fn insights_document(repo: &str) -> Value {
    json!(crate::insights::snapshot(repo))
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
    // `?` is the repository boundary: an untrusted, missing or unrelated path
    // is refused whole. A ledger that exists but cannot be read is not that —
    // `status_readonly` reports it inside the ledger facet (`recording: false`
    // plus `error`), so the worktree and code-graph facets still answer.
    let ledger_status = crate::ledger::bindings::repository_status_readonly(repo)
        .map_err(|error| error.to_string())?;
    let codeintel_status = crate::codeintel::status(repo);
    let worktrees = match crate::engine::worktree::list_worktrees_scanned(
        repo,
        crate::engine::worktree::ScanDepth::Dirty,
    ) {
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

pub(super) fn repo_facet(facet: &str, repo: &str) -> Result<Value, ReadError> {
    match facet {
        "insights" => Ok(insights_document(repo)),
        "status" => status_document(repo).map_err(ReadError::Internal),
        "collisions" => Ok(json!(crate::insights::collision_risk(repo))),
        "elsewhere" => Ok(json!(crate::insights::dirty_elsewhere(repo))),
        "changes" => Ok(json!(crate::insights::active_changes(repo, None, None))),
        "context" => Ok(json!(crate::insights::change_context(repo, None))),
        "ledger" => {
            let (address, caveat) = crate::ledger::bindings::repository_read_target(repo)
                .map_err(|error| ReadError::Internal(error.to_string()))?;
            ledger_query_page(
                &address.anchor,
                &crate::ledger::EventQuery {
                    limit: super::LEDGER_DEFAULT_LIMIT,
                    ..crate::ledger::EventQuery::default()
                },
                caveat,
            )
            .map_err(ReadError::Internal)
        }
        "tasks" => {
            let address = crate::ledger::bindings::repository_address_readonly(repo)
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

    /// The frame is measured on the wire, where JSON escaping grows a quote or
    /// a control character 2-6x. A document capped on its raw length always
    /// overflowed the frame once escaped, which ended the session.
    #[test]
    fn a_document_that_escapes_to_more_than_the_frame_is_delivered_truncated() {
        const FRAME: usize = 4 * 1024 * 1024;
        for payload in [
            "\"".repeat(3 * 1024 * 1024),
            "\u{1}".repeat(3 * 1024 * 1024),
            "é".repeat(3 * 1024 * 1024),
            "😀".repeat(1024 * 1024 + 7),
        ] {
            let contents = vec![text_content(
                "gitpulse://insights/tmp/x",
                json!({ "blob": payload }),
            )];
            let response = super::super::JsonRpcResponse {
                jsonrpc: "2.0",
                id: json!(1),
                result: Some(json!({ "resultType": "complete", "contents": contents })),
                error: None,
            };
            let wire = serde_json::to_string(&response).unwrap().len() + 1;
            assert!(
                wire <= FRAME,
                "a truncated document is {wire} bytes on the wire"
            );
            assert!(
                contents[0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("… truncated"),
                "the truncation is not announced"
            );
        }
        // The manifest is far below the cap and stays whole JSON.
        let manifest = read("gitpulse://server/manifest").expect("manifest reads");
        assert_eq!(manifest[0]["mimeType"], "application/json");
    }

    /// A row written before the append cap existed can hold a megabyte in one
    /// field. The page that carries it is bounded per row, not per page.
    #[test]
    fn a_ledger_row_stored_before_the_cap_comes_back_capped() {
        use crate::ledger::{self, ActorKind, Draft, Outcome};
        use crate::procguard::LockedSpawn;
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().to_string_lossy().into_owned();
        let init = std::process::Command::new("git")
            .args(["init", "-q", &repo])
            .output_locked()
            .expect("git init");
        assert!(init.status.success(), "git init failed");
        crate::test_support::trust_repo(dir.path());
        let anchor = ledger::bindings::repository_address(&repo)
            .expect("address")
            .anchor;
        let heavy = json!({ "note": "n".repeat(1024 * 1024), "keep": "small" }).to_string();
        ledger::record(Draft {
            repo_path: anchor.clone(),
            action: "session.command".into(),
            actor_kind: Some(ActorKind::Agent),
            outcome: Some(Outcome::Ok),
            argv_json: Some(json!(["echo", "a".repeat(512 * 1024)]).to_string()),
            detail_json: Some(heavy),
            ..Default::default()
        })
        .expect("recorded");

        let page = ledger_page(&anchor, 0, 10).expect("page");
        let size = page.to_string().len();
        assert!(size < 128 * 1024, "one heavy row made a {size} byte page");
        let event = &page["events"][0];
        let argv: Value = serde_json::from_str(event["argv_json"].as_str().unwrap())
            .expect("argv_json stays JSON");
        assert!(argv.is_array(), "argv_json stays an array: {argv}");
        let detail: Value = serde_json::from_str(event["detail_json"].as_str().unwrap())
            .expect("detail_json stays JSON");
        assert_eq!(detail["keep"], "small");
        assert!(
            detail["gitpulse_truncated"]["keys"]
                .as_array()
                .is_some_and(|keys| keys.iter().any(|key| key == "note")),
            "the cut names what it removed: {detail:.200}"
        );
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
        // The task-board tools write the task profile, so the surface is not
        // read-only; it never writes git, and it names exactly what writes.
        assert_eq!(parsed["readOnly"], false);
        assert_eq!(parsed["gitWrites"], false);
        assert_eq!(
            parsed["writes"],
            json!([
                "gitpulse_add_task",
                "gitpulse_import_tasks",
                "gitpulse_complete_task",
                "gitpulse_delete_task",
                "gitpulse_merge_tasks"
            ])
        );
        assert_eq!(
            parsed["storeSchemaVersion"],
            crate::codeintel::SUPPORTED_STORE_SCHEMA
        );
        assert_eq!(parsed["dcStoreSchemaVersion"], dc_store::SCHEMA_VERSION);
        assert_eq!(
            parsed["workbenchSchemaVersion"],
            dc_store::workbench::WORKBENCH_SCHEMA
        );
        // The number the linked extractor's string constant names, not a copy.
        let extract: u64 = devmap_extract::cache::EXTRACTION_SCHEMA_VERSION
            .parse()
            .expect("the extraction schema is a whole number");
        assert_eq!(parsed["extractionSchemaVersion"], json!(extract));
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

    fn trusted_repo() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        crate::test_support::git_in(dir.path(), &["init", "-q"]);
        crate::test_support::trust_repo(dir.path());
        let repo = dir.path().canonicalize().expect("canonical");
        let repo = repo.to_string_lossy().into_owned();
        (dir, repo)
    }

    fn record_events(repo: &str, count: usize) {
        for _ in 0..count {
            crate::ledger::append(crate::ledger::Draft {
                repo_path: repo.to_string(),
                action: "git.commit".to_string(),
                actor_kind: Some(crate::ledger::ActorKind::Human),
                outcome: Some(crate::ledger::Outcome::Ok),
                ..Default::default()
            })
            .expect("append");
        }
    }

    #[test]
    fn the_ledger_resource_pages_like_the_tool() {
        let (_dir, repo) = trusted_repo();
        record_events(&repo, 60);
        let page = repo_facet("ledger", &repo).expect("ledger facet");
        assert_eq!(page["returned"], 50, "{page}");
        assert_eq!(page["truncated"], true, "{page}");
        let next = page["next_cursor"].as_i64().expect("next_cursor");
        assert_eq!(next, 50);
        let rest = ledger_page(&repo, next, 50).expect("second page");
        assert_eq!(rest["returned"], 10);
        assert_eq!(rest["truncated"], false);
    }

    #[test]
    fn an_exactly_full_final_page_is_not_truncated() {
        let (_dir, repo) = trusted_repo();
        record_events(&repo, 3);
        let full = ledger_page(&repo, 0, 3).expect("page");
        assert_eq!(full["returned"], 3);
        assert_eq!(full["truncated"], false, "{full}");
        let short = ledger_page(&repo, 0, 2).expect("page");
        assert_eq!(short["truncated"], true, "{short}");
        assert_eq!(short["next_cursor"], 2);
    }

    #[test]
    fn reading_the_ledger_resource_creates_nothing() {
        let (dir, repo) = trusted_repo();
        let page = repo_facet("ledger", &repo).expect("ledger facet");
        assert_eq!(page["ok"], false, "{page}");
        assert!(
            !dir.path().join(".devcouncil").exists(),
            "a resource read created the ledger"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_ledger_that_cannot_be_read_degrades_one_facet_not_the_status() {
        let (dir, repo) = trusted_repo();
        let state = dir.path().join(".devcouncil");
        std::fs::create_dir(&state).expect("state dir");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(
            state.join("ledger.sqlite"),
            b"this is not a sqlite database at all",
        )
        .expect("corrupt ledger");
        let status = status_document(&repo).expect("the status call must not fail whole");
        assert_eq!(status["ledger"]["recording"], false, "{status}");
        assert!(
            !status["ledger"]["error"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "{status}"
        );
        assert!(status["worktrees"].is_object(), "{status}");
        assert!(status["codeintel"].is_object(), "{status}");
        // A ledger that could not be read is never reported fresh.
        assert_eq!(status["ledger"]["recording_state"], "unknown", "{status}");
        assert_eq!(status["ledger"]["stale"], true, "{status}");
        assert_eq!(status["ledger"]["freshness_ok"], false, "{status}");
        assert!(status["ledger"]["as_of"].is_null(), "{status}");
    }

    #[test]
    fn the_status_ledger_facet_is_readable_never_recording_and_labels_dropped() {
        let (_dir, repo) = trusted_repo();
        record_events(&repo, 1);

        let status = status_document(&repo).expect("status");

        let ledger = &status["ledger"];
        assert_eq!(ledger["recording"], false, "{ledger}");
        assert_eq!(ledger["recording_state"], "readable", "{ledger}");
        assert_eq!(ledger["dropped_scope"], "this_process", "{ledger}");
        assert!(ledger["as_of"].is_string(), "{ledger}");
        assert_eq!(ledger["stale"], false, "{ledger}");
    }
}
