# Workbench data-layer measurements

## 2026-09-09 query checkpoint

The 10,000-task fixture now measures scoped search and durable edits as well as
the original five read cases. The active module selects Go 1.26.6; the host is
Apple M5 Pro, 18 logical CPUs, 64 GiB RAM, macOS 27.0 (26A5425a). The real Go
client still uses its existing content-addressed debug Rust binary. No full
repository gate overlaps the measured run; normal host activity is uncontrolled.
Each case has 100 samples, with p95 reported from the 95th ordered sample.

| Operation | Mean (ms) | p95 (ms) |
|---|---:|---:|
| Global, first 200 cards | 4.064 | 7.129 |
| Workspace, 1,900 matches / first 200 | 15.209 | 19.60 |
| Repository, 100 matches | 1.698 | 2.243 |
| Broad global search, 10,000 matches / first 200 | 18.906 | 23.93 |
| One task's ten revisions | 1.147 | 3.248 |
| Rare global search, one match | 0.756 | 2.430 |
| Repository-scoped broad search, 100 matches | 10.704 | 14.18 |
| Workspace-scoped broad search, 1,900 matches / first 200 | 27.062 | 36.77 |
| Empty status column | 0.401 | 1.238 |
| Committed task text edit | 1.764 | 3.151 |

These are data-boundary timings, not desktop qualification. The benchmark checks
exact totals and shown counts before timing. Edits change saved text, and the
final event count must increase exactly once per mutation, including calibration
iterations. Setup, correctness reads and result checking are outside timing.
No cached fixture or raw-SQL seed substitutes for the public mutation API.

### 100,000-task stress fixture

The same benchmark then created 100,000 tasks with one revision each, 100
repositories, ten workspaces and 100,110 initial events. Workspace queries match
19,000 tasks; repository queries match 1,000. Queries still return at most 200
cards. All 100-sample cases completed and the command exited successfully after
560.679 seconds, including both fixtures' setup and cleanup.

| Operation | Mean (ms) | p95 (ms) |
|---|---:|---:|
| Global | 9.445 | 13.76 |
| Workspace | 154.335 | 212.8 |
| Repository | 13.873 | 21.45 |
| Broad global search | 218.987 | 484.5 |
| One task's single revision | 0.840 | 1.973 |
| Rare global search | 0.706 | 1.560 |
| Repository-scoped broad search | 130.453 | 313.5 |
| Workspace-scoped broad search | 305.102 | 527.8 |
| Empty status column | 0.553 | 1.466 |
| Committed task text edit | 1.484 | 2.693 |

**Stress-scale search does not meet the 100 ms target.** The command's PASS
verifies benchmark execution and result correctness; it does not enforce the
product latency budgets. Broad matching sets and workspace selection still need
profiling, including count/page costs and metadata reads. These measurements do
not prove that a proposed index or cache will solve the remaining bottleneck.
Whole-process memory, idle CPU, native rendering/drag, cold/warm navigation and
the eight-hour soak remain unmeasured. Go allocation traffic is not retained
memory, and the debug data-boundary benchmark is not a release desktop result.

Reproduce both current fixtures from Manvi's `manvi/` directory:

```sh
go test ./dc/store -run '^$' -bench '^BenchmarkWorkbenchProfile(Stress)?$' -benchtime=100x -count=1 -timeout=20m
```

### Diagnosis and regressions

- A fresh pre-change 100-sample baseline did **not** reproduce the historical
  171.4 ms search p95 below: it measured 21.93 ms. Final global search is 23.93 ms;
  this run does not establish an improvement for that case. Repository reads
  changed from 17.87 to 2.243 ms p95, and workspace reads from 39.16 to 19.60 ms.
- Optional-filter OR branches made selective counts visit unrelated task rows.
  At 10,000 tasks, the original status/repository/workspace/rare-search counts
  took 70,051 / 190,063 / 249,813 / 100,132 SQLite VM operations. Explicit indexed
  predicates reduce those counts to 53 / 94 / 637 / 74. All four original
  regression thresholds remain unchanged.
- Sorting FTS candidates read bodies that were later discarded. A projection
  tripwire failed before the fix with `malformed JSON` from an unselected row.
  It now passes: IDs and order are selected before body formatting, at most one
  page plus lookahead. This uses SQLite's documented
  [materialization fence](https://www.sqlite.org/lang_with.html#materialization_hints).
- The first scoped-search optimization was wrong: repeatedly opening MATCH per
  member measured 233.4 ms p95 for only 100 repository matches. The expanded run
  was deliberately interrupted before its stress fixture; it is **not a pass**.
  A retained regression separately reproduced 12.47 seconds for 9,990 matches.
  Scoped queries now build their FTS hit set once; the repository benchmark is
  14.18 ms p95. The gross-regression test allows two seconds to avoid treating it
  as a hardware-specific 100 ms product gate.
- VM steps omit virtual-table callback work. The original 5,000-step limit for
  scoped common-term queries rewarded the slower correlated plan. Those two
  cases now allow one indexed hit-set pass plus fixed query work; a separate
  elapsed-time regression and host benchmarks check the cost the counter misses.
  The full store suite passes 93 tests, including all 64 scope/status/search
  combinations against an independent reference, paginated small and large
  result sets, changed links, overlapping groups, renamed text and deletion.

The broader search strategy and instrumentation mistakes are retained here so
future changes are compared with both sparse and dense cases. Do not infer that
LiquiTask's historical lag had these same causes.

Captured outputs: `/tmp/manvi-workbench-performance-before.log`,
`/tmp/manvi-workbench-performance-qualified.log`,
`/tmp/manvi-search-projection-before.log`,
`/tmp/manvi-scoped-search-before.log`, `/tmp/manvi-query-store-final.log`.
The temporary fixture from the interrupted run was removed only after its child
exited and an open-handle check found no remaining reader or writer.

### Integration gates after measurement

GitPulse `npm run ci:local` passed with 5,225 frontend tests (one skipped), 24
browser regressions, 2,068 Rust tests (ten ignored) and all coverage/build/type/
format/Clippy gates. Frontend line/branch coverage is 96.06%/88.82%; Rust line
coverage is 84.64%. One ignored native-to-Go workbench test was subsequently run
with current binaries and passed. Other opt-in timing, deep-fuzz, live-update
and real-repository suites remain outside this run.

Manvi's full `verify.sh` completed with a qualified pass: 82.1% Go coverage,
184 Rust tests and 869 benchmark-instrument checks. Five Go cases skipped;
golangci-lint, govulncheck, nilaway, incumbent lease interoperability, the stale
flat navigation artifact and live cloud-provider checks remain gaps. Its live
local-provider wire probe and focused Go store/serve/CLI race suites passed.
The vendored store source hashes match Manvi and the vendor manifest.

Gate logs: `/tmp/gitpulse-query-ci.log`, `/tmp/gitpulse-query-real-profile.log`,
`/tmp/manvi-query-verify.log`, `/tmp/manvi-query-race.log`. The successful gates
do not resolve the stress latency failures or qualify installed native behavior.

## Historical 2026-09-08 baseline

Measured on 2026-09-08 from the isolated Manvi worktree. This is a baseline for
the implementation plan, not desktop performance qualification.

## Fixture and method

- Apple M5 Pro, 18 logical CPUs, 64 GiB RAM, macOS 27.0; Go 1.26.6.
- Real Go workbench client and persistent Rust `dcstore` child, using the debug
  binary selected by the existing test helper. No mock transport or raw-SQL seed.
- 100 repositories, 10 workspaces, 10,000 tasks, ten revisions per task,
  100,110 committed events. The benchmark asserts the final event count.
- Tasks have a 552-byte description and stable titles/positions across revisions.
  Each has one repository link and a home workspace. Workspace queries exercise
  the union of home tasks and tasks in member repositories.
- Setup uses the public transactional API and is excluded from query timing.
  Each query has 20 measured iterations; the reported p95 is the 19th ordered
  sample. This small sample is preliminary and provides no confidence interval.
- The run overlapped repository verification and normal system activity. It was
  not an isolated, release-build comparison. Total benchmark process time was
  421.705 seconds, including fixture setup and cleanup.

## Results

| Query | Mean (ms) | p95 (ms) | Allocated bytes/op | Allocations/op |
|---|---:|---:|---:|---:|
| Global, first 200 cards | 16.33 | 58.96 | 489,209 | 555 |
| Workspace, first 200 cards | 50.09 | 99.53 | 489,371 | 556 |
| Repository, up to 200 cards | 95.01 | 224.90 | 245,327 | 353 |
| Search `startup latency`, first 200 cards | 94.12 | 171.40 | 489,347 | 557 |
| One task's ten revisions | 1.39 | 4.16 | 85,459 | 165 |

In this historical run, search exceeded the plan's 100 ms target at the data
layer alone. Repository queries also left little time within the 300 ms
warm-switch budget for IPC and rendering. The cause was unverified at that
checkpoint. The investigation above found query defects, but does not attribute
all of this earlier run's latency, or LiquiTask's historical lag, to those defects.

Allocated bytes are per-operation Go allocation traffic, not retained memory or
the whole process tree. This historical run did not measure cold board rendering,
interactive edits, drag latency, idle CPU/memory, 100k-task stress, long-running
growth or eight-hour soak. Native UI, notifications and managed agents were also
outside this test. See the newer checkpoint above for current coverage.

## Reproduce

From Manvi's `manvi/` directory:

```sh
go test ./dc/store -run '^$' -bench '^BenchmarkWorkbenchProfile$' -benchtime=20x -count=1 -timeout=10m
```

The fixture is temporary and removed after the client closes. The test source is
`manvi/dc/store/workbench_bench_test.go`; local captured output is
`/tmp/manvi-agentic-workspaces-benchmark.log`. Keep build mode and competing work
explicit when comparing subsequent runs.
