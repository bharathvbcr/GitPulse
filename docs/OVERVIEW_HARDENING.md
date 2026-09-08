# Workspace Overview hardening

Scope: Workspace → Overview (`WorkView.svelte`) and its projection, loader,
refresh lifecycle, and repository-opening destination. The working checkout
contains concurrent changes to other features; this is not a release audit.

## Contracts and regression evidence

| Contract | Implementation | Verification |
| --- | --- | --- |
| A failed or superseded open cannot redirect the active repository | `repoStore.openRepo` runs `onReady` only after canonical resolution and successful hydration, while the same request, session, view and selection still own navigation | Store regressions cover resolution/hydration errors, aliases, later opens, later views and a file selected during hydration; browser checks cover failed opens and Resolve |
| Local status does not depend on having a branch | `hereSummary` keeps detached-checkout counts and excludes bare repositories | Unit and browser checks for detached review and bare state |
| Unknown is different from clean | `dirtySummary` carries measured/total counts; invalid counts stay unmeasured; operation probe coverage is explicit | Mixed clean/unscanned task, invalid numeric values, bare repository and failed-probe cases |
| Joined counts do not multiply source identities | Projection deduplicates worktree/PR/run identities; summary counts unique PR numbers | Duplicate records and shared-branch regressions |
| Parked operations cannot be buried by activity volume | Operation priority precedes the activity weight and unbound ordering | A blocked unbound row remains ahead of a task with 2,000 PRs |
| Incomplete data remains visible as incomplete | Source errors precede absence; GitHub warnings and PR/run truncation are retained; malformed responses degrade only their source | Regressions initially reproduced 17 source/projection/status failures; failed-source browser state is distinct from an empty workspace |
| Failed overlap probes do not discard known overlaps | The failure notice and the returned overlap list render independently | Native collision tests use real temporary Git repositories; browser partial-data fixture follows the native `ok: false` response shape |
| Refresh work stays bounded and cannot publish after cancellation | One running refresh plus the latest queued request; cooperative cancellation; shared load deadline; independent collision deadline | 10,000 update requests coalesce to two loads; switch/close/unmount, hung scan and late-answer tests |
| A cache is visibly a previous snapshot | Cached projection retains its load time; refresh remains busy through the overlap scan | Browser remount and delayed-overlap checks |
| Large inputs cannot expand into an unlimited join or DOM | IPC collections/text are validated and bounded; GitHub associations cap at 20,000 with included/total counts; rows render 100 at a time | 250,000-association adversarial case; 250-worktree browser pagination and search beyond the rendered page |
| Status labels describe the available evidence | Agent **worktrees** count paths; PRs show reported CI/review state; run chips show the latest returned run by branch and workflow name | Browser partial-GitHub checks and out-of-order historical-run unit tests |

The Needs attention filter includes parked operations, measured changes,
unmeasured worktrees/operations, reported PR failures or requested changes,
and failed latest observed runs. Search still covers every loaded row when
only the first page is rendered.

## Bounds and limits

- A load has one 20-second deadline, including secondary probes. Collision
  scanning has a separate 20-second wait, after rows can be published.
- Operation probes: 32; task bindings: 64; task titles: 32. Secondary IPC
  fan-out uses the existing pool (4). Incomplete coverage is reported.
- Response validation permits at most 10,000 records per collection and
  65,536 characters per consumed text field. Oversized sources fail visibly.
- Ledger tallies are explicitly described as recent events, within the
  requested tail of 500 records.
- Cancellation prevents further scheduling and stale publication. A frontend
  timeout stops waiting; it does not kill a native IPC command already running.
  Native Git commands have their own process deadlines and concurrency gate.
- Workflow run payloads identify runs, branches and workflow names. Grouping
  by name is a display summary of returned runs, not proof that all workflows
  ran against the current commit. GitHub truncation warnings remain visible.

## Reproduction

Run the commands in [the preview guide](../harness/overview.md). Unit coverage
lives under `src/lib/work`, `WorkView.test.ts`, and the repository-store tests.
The production Svelte component is mounted in the browser harness through the
real repository store and an explicit command fixture table.

No dependency was added. Overview adds no Git mutation commands. Opening external PR/run links continues through the existing opener.
No commit, release, application installation, or signing change is included.

## Verification completed on 2026-09-07

| Check | Result |
| --- | --- |
| Full frontend suite snapshot (`npm test`) | 5,049 tests passed across 388 files |
| Final focused suite after the missing-data metric correction | 255 tests passed across 9 files |
| Final `npm run check` | Passed; zero Svelte errors or warnings; Node TypeScript check passed |
| IPC and wire contracts | Passed; 187 handlers, 49 wire contracts, no drift |
| Final production build | Passed; existing 500 kB chunk-size advisory remains |
| Native collision tests | 2 passed; 1,460 unrelated native tests filtered out |
| Browser fixture matrix | 74 checks passed across all 11 documented scenarios |
| Responsive inspection | Dark/light at 600 px, including partial data; long-path pane measured 590 px client/scroll width with a 600 px document |
| Scoped whitespace/diff check | Passed |

Earlier full-suite failures in concurrent ConflictEditor/graph edits cleared
in the later full-suite snapshot. Their edits were preserved.

Not run: the complete `ci:local` Rust lint/coverage pipeline, an installed-app
webview smoke test, release/signing checks. This change targets the frontend;
native verification was limited to the collision contract it consumes.

Overview-owned files, including tests, harness and this record: **+1261 / -197
lines** against HEAD. The mixed-ownership repository-store files are excluded
from that count; their changes here add the guarded navigation callback.
