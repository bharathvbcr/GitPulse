# Manvi task extraction and enhancement audit

Audit date: 2026-09-09. Scope: notes extraction, task editor layout, inline and
Quick Enhance review, IPC reconciliation, native/store integration, and Manvi's
model-output decoder. Evidence below distinguishes browser fixtures from real
processes and inference. This is bounded verification, not proof that every
provider or model preserves meaning for every prompt.

## 2026-09-11 follow-up: notes-to-task extraction still failed after rewrite hardening

The 2026-09-09 work treated Manvi as a field rewriter. Notes-first drafting still
failed in three places, each reproduced against unmodified code before the fix:

| Finding | Pre-fix evidence | Resulting behavior |
| --- | --- | --- |
| Local title extraction split on list numbers and abbreviations | `titleFromNotes("1. Fix the auth bug in both repos")` returned `"1."`; `"Dr. Smith reported E42 on login."` returned `"Dr."` | Title extraction skips weak clauses (`1.`, `Dr.`, `e.g.`), heading markers, and checkbox bullets. A weak first line falls through to the next non-empty line. |
| Notes-derived titles froze Manvi rewrites | Notes `Must keep E42. Do not drop the reproduction steps.` became the title; decode then rejected `Preserve E42 reproduction` with `model altered a protected constraint in title` | Title constraints that also appear in the description stay protected in the description only. Title-only constraints that are not in the description remain frozen. Identifier literals such as `E42` stay required in the title. |
| Thinking models and chat preambles were refused | `<think>…</think>` then JSON was an explicit refusal; `"Sure, I can help.\\nHere is the JSON:"` plus an object also failed | Complete `<think>` / `<thinking>` wrappers, leftover close tags, multi-line chat preambles, and `JSON: {…}` unwrap to one object. Unclosed think, think without JSON, trailing prose, extra keys, and a second object still fail. Raw wrapping may be up to 256 KiB; the JSON object remains capped at 72 KiB. |
| Cmd+Enter failed with no message when Ask was gated | `generate()` returned when `askDisabled && !quick` | The same gate now writes `gate` / `fieldReason` / a live-attempt message into the assist error. |

Verification (this follow-up): GitPulse focused workbench tests 70 passed, including a 200-item numbered-list corpus and abbreviation cases. Manvi decoder unit tests passed, including 10 race repetitions. Fuzz: `FuzzEnhancementOutputCannotSmuggleUnrequestedFields` 277,482 executions in 20s, passed; `FuzzEnhancementUnwrapKeepsSingleObjectContract` 1,547,001 executions in 20s, passed. Browser harness, signed app install, and live-model inference were not repeated here; GitPulse also has unrelated uncommitted UI work outside this path.

Security impact: the accepted model-output envelope expands only to complete think wrappers and a leading `{` after those wrappers. Extra keys, trailing tokens, tools, and duplicate keys remain refused. No credentials, authorization, or provider-resolution changes.

## Delivery locations

- GitPulse: `/Users/bharath/.codex/worktrees/manvi-task-hardening/GitPulse`,
  branch `codex/manvi-task-hardening`, originally based on `2a3c438`.
- Manvi: `/Users/bharath/.codex/worktrees/manvi-task-hardening/Manvi`,
  branch `codex/enhancement-json-envelope`, based on `28610bd`.
- At initial audit completion, changes were uncommitted and installed GitPulse
  and Manvi had not been replaced. The subsequent local installation uses these
  branches; exact build commits, backups and installed verification belong to
  its separate installation receipt.

The 2026-09-10 local build integrates these changes onto GitPulse `f90696c`.
That main revision already contains most of the original extraction/layout
repairs; the final branch adds the remaining confirmation checks, regressions
and documentation while preserving the newer platform and release fixes.

Concurrent work modified and stashed the shared GitPulse checkout during this
audit. Relevant task changes were recovered into the isolated worktree without
popping or deleting the shared stash. Existing task layout/extraction groundwork
was preserved and extended; it is included in the reviewed result.

Production code across the two repositories adds 304 lines and removes 345 lines
(41 fewer overall); tests and documentation are counted separately.

## Verified findings and fixes

| Finding | Evidence and resulting behavior |
| --- | --- |
| Missing model blocks the screenshot's action | The installed Manvi's `work.enhancements.configuration` returned provider `anthropic`, empty model, model source `none`. The task configuration is separate from the application header selection. `TaskManviAssist.svelte` now exposes provider/model selection and configuration reload inline, and refuses generation before configuration is ready. |
| Notes-only Save could not reach extraction | A new browser regression failed because the required title input prevented form submission. The editor now performs explicit validation after extraction, so notes alone can supply the title. |
| Notes could replace existing description or be applied again after accepting improved wording | Regression tests characterize appending existing description plus notes and consuming notes once. `taskCompose.ts` owns this behavior; Save and prepare-for-Manvi use it. Unsaved agent copy includes extraction without modifying the draft. |
| Long input could be silently truncated and title truncation could split emoji | Extraction retains full text, checks the combined UTF-8 byte limit before applying or clearing notes, and caps derived titles by Unicode code point. Saving supports a 64 KiB description; model generation retains its separate 32 KiB source bound. |
| Editor form and lower sections overlapped | The form could shrink inside the scrolling flex column. A dedicated `.sheet-body` now scrolls the form, enhancement history and runs in normal flow. The sticky save bar is opaque so scrolled text cannot bleed through it. Native notification task layout uses the same scroller contract. |
| Inline generation could repeat through keyboard input | Button and Ctrl/Cmd+Enter use the same gate. A running proposal blocks generation; saved history is checked before requesting another suggestion. Empty or locked field choices cannot silently expand to all fields. |
| Accepting a suggestion could overwrite unsaved edits | Inline acceptance requires clean saved fields and a matching source revision and task identity. Backend optimistic revision checks remain authoritative across other editors or hosts. |
| A lost mutation reply could lose its retry identity | `EnhancementAction` now owns the receipt and immutable input snapshot across inline, history and Quick Enhance. Uncertain replies retain the exact request; confirmed acceptance followed by a failed refresh retries only the read. Mutations and returned task identities/revisions are checked before updating the editor. |
| Separate enhancement surfaces could clear each other's lock | The editor keeps independent inline/history busy states and combines them. Pending reconciliation blocks editing and closing while leaving the correct retry control usable. |
| Polling through a capped history page could miss the active proposal | Active proposals are read directly by ID. Disposal, visibility, request epochs and identity checks protect the inline poll; history selection updates the matching cached entry. History pagination still reports its supplied totals rather than claiming complete coverage. |
| Quick Enhance duplicated lifecycle code | The sheet now hosts the existing `TaskEnhancements` review component. Its task load stays bounded and it respects the component's pending-action close lock. |
| A real model response failed despite containing valid proposal JSON | The Seattle prompt failed after 18.14 seconds with `enhancement response must be a JSON object`. Inspection of the synthetic local response showed one complete JSON Markdown fence. Manvi now accepts raw JSON or exactly one complete unlabeled/JSON fence, while refusing prose, multiple fences, other languages and malformed objects. |

## Invariants retained and attacked

- Only explicit acceptance changes task fields. Generating a suggestion does not
  complete a task, launch an agent, grant tools or write to a repository.
- A request receipt identifies one mutation. Lost create/generate responses do
  not authorize another inference. A confirmed mutation followed by a failed read
  is reconciled through a read, not another mutation.
- Dirty drafts and foreign/stale task or proposal identities cannot be accepted.
  Duplicate clicks are bounded at the frontend; backend receipts and revisions
  remain the cross-process authority.
- Notes are retained on oversized-input refusal. Existing description is retained
  on extraction. Consumed notes cannot overwrite accepted wording on later saves.
- Every IPC wait uses the existing timeout contract on the changed paths. A
  timed-out mutation remains uncertain and retryable; timeout is not cancellation
  proof and never means the write failed.
- Model output remains bounded before unwrapping. Existing duplicate-key,
  unknown-key, requested-field, Unicode, literal/constraint and completion-claim
  checks still apply inside the envelope. Request and host-command JSON parsers
  were not changed to accept Markdown.
- Security impact: the accepted model-output envelope expands narrowly; execution
  permission, authorization, credentials, quotas and provider resolution do not
  change. No dependency or saved provider configuration was added or modified.

## Verification record

New regression cases were run against unchanged relevant code before fixes:
notes preservation, oversized extraction, emoji boundaries, notes-only Save,
duplicate keyboard generation, dirty acceptance, lost inline receipt recovery,
opaque footer rendering, and fenced JSON output. Each produced its expected
failure before its corresponding fix.

| Verification | Result |
| --- | --- |
| GitPulse `npm test` | 442 files passed; 5,779 tests passed, 1 skipped, 5,780 total. The optional local `docs/PROMO.md` count check was skipped because that document is absent from this worktree. |
| Focused task helpers/components | 249 tests passed in the final focused run, including 16 adversarial transaction cases. |
| `npm run check` | Passed: Svelte 0 errors, 0 warnings; TypeScript passed. |
| `npm run build` | Passed. Vite reports the existing large-chunk advisory. |
| `npm run test:browser -- --harness tasks` | Chrome: 95/95 passed, increased from the 81-check baseline. |
| `npm run test:webkit -- --harness tasks` | WebKit: 95/95 passed. |
| `npm run test:browser -- --harness task-materials` | 59/59 passed. |
| Direct browser inspection | Opened and saved a notes-only fixture task. Final saved-task rendering shows distinct sections, scrolling and an opaque save bar. This is a browser fixture, not the installed app. |
| Native `cargo test --manifest-path src-tauri/Cargo.toml --lib workbench:: --jobs 4` | 29 passed, 3 explicitly ignored. The real-profile test was then enabled separately and passed. |
| Native real-profile host/store integration | `real_profile_host_shares_native_revisions_and_refuses_a_stale_generation` passed with the rebuilt Manvi and installed dcstore. |
| Store workbench/enhancement/automation suites | 12 + 15 + 13 = 40 passed. |
| Manvi `go test ./serve ./cmd/manvi -run 'Enhancement\|Workbench\|ServeGenerates' -count=1` | Both packages passed. Run from the `manvi/` module directory. |
| Manvi `go test -race ./serve -run 'Enhancement\|Workbench' -count=10` | Passed all ten repetitions under the race detector. |
| Decoder fuzzing | Unchanged decoder: 948,609 executions. Fixed decoder: 45,702 executions in a separate 30-second run, passed. These are separate campaigns, not one count of fixed-code coverage. Target: `FuzzEnhancementOutputCannotSmuggleUnrequestedFields`. |
| Git diff whitespace checks | Passed in both isolated worktrees. |

The transaction cases cover four uncertain error classes, lost create/generate
receipts, read-only reconciliation after acceptance, a 100-click concurrency
burst, timeout with a retained receipt, forged or stale confirmations, definite
revision conflicts, and empty/locked field requests. Browser checks also exercise
a 100-event keyboard burst, missing configuration recovery and pending inline
acceptance retry. Decoder tests cover JSON fences with/without language, CRLF,
leading/trailing prose, multiple fences, wrong language, incomplete fences,
escaped duplicate keys, foreign fields, lost error literals and oversized padding.

The initial full-suite rerun correctly rejected the opaque save bar under the
old blanket translucent-material test. That test now explicitly requires the
sticky save bar to be opaque and retains the original material checks for every
other surface; the visual regression continues to check actual computed opacity.

## Verified live Seattle task

The rebuilt `/tmp/gitpulse-task-manvi` used a disposable database and explicit
process-local `local` / `gemma4:e4b-it-q4_K_M` configuration on the existing local
endpoint. No user tasks or saved configuration were changed.

Input: `Prepare Demo for Seattle start-up event`.

Generation became `ready` after 18.07 seconds. Suggested title:
`Finalize Demonstration for Seattle Startup Event`. The description proposed
preparing presentation slides, scripts and technical setup. The task remained at
revision 1 until explicit acceptance. Sending the same acceptance receipt twice
persisted exactly one new revision, revision 2, with the accepted text.

This verifies a real inference and durable acceptance path for this model. The
added detail in the suggestion also demonstrates why human review remains
necessary: valid JSON and literal preservation do not prove semantic equivalence.

## Remaining verification limits and unrelated failure

- **Verified unrelated failure:** broad Manvi `go test ./...` fails in
  `computer.TestHumanControlClickReobservesUnchangedFormBeforeInput`:
  `human_admission_test.go:145: trusted control click was refused against the stale HID baseline: &{ActionID:manual-proof Delivery:not_sent DispatchedAtMillis:0 Verified:false}`.
  The identical test fails 20/20 repetitions on unchanged Manvi main. It belongs
  to computer-control admission, outside task enhancement, and was not altered.
  All other packages in that full run passed.
- **Not run:** the two remaining ignored native checks require a managed Codex
  model turn or installed Claude/Codex CLI help probes. They test coding-agent
  launch modes, not task suggestion generation. Full native lint/coverage/release
  CI, a signed app rebuild/install, and physical installed-app verification were
  not performed in this task.
- **Unverified:** live cloud providers, every available local model, cross-machine
  transport behavior, and universal semantic preservation. Synthetic error and
  race tests do not certify arbitrary external providers.
- DevMap stores were rebuilt in both worktrees and report fresh generation 3
  (GitPulse) and 2 (Manvi), no parse failures and no quarantined paths. Impact walks were depth-limited
  and reported unresolved attribution; source inspection and full relevant tests
  supplement them. GitNexus's final diff analysis reports medium scope, with its
  visible affected flows in material checks. Its Svelte coverage is incomplete;
  graph output is not a complete enumeration of UI callers or a proof of safety.

Detailed session logs are under `/tmp/gitpulse-*` and are temporary; the concrete
commands, failures, counts and limits above are the durable audit record.
