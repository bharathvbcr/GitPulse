# Load governor

GitPulse runs agents. On the Mac this was built on, agents and the tools they
start brought the machine down three times in one week. The host-load governor
is GitPulse's answer, built as a series of cards. This page describes the
series and what each card owns. It then covers the part that exists today, the
**sensor** (card 1): what it measures, how other processes read it, where its
limits are, and what has been verified.

## Scope

- **macOS is governed.** The sensor's readings are complete there, and the
  cards that consume them may refuse, defer or shed work on them.
- **Linux is observe-and-advise through PSI.** The sensor reads
  `/proc/pressure/memory` and `/proc`. The PSI-to-level mapping has **not been
  verified on a real Linux host**, so Linux never governs; it advises.
- **Windows reports unavailable.** No sensor runs there. Readers see the
  sensor as dead, and Health says "ungoverned: no sensor".
- **Cursor and Codex get advice only.** Their sessions are counted (Codex by
  executable), but GitPulse holds no lease over them.
- **"Time slots" means a priority queue with leases, not calendar booking.**
  That queue is card 1d's.

## What the crashes were

From the forensics of 2026-10-09 (V = verified, I = inferred):

- **Three real memory crises, each one runaway process.** A `go test` at
  120 GiB (V), a Python at 74.8 GiB that ended in a reboot (I: likely in
  GitPulse.app's coalition), and an `rg` memguard killed at 40.8 GiB (V, its
  log).
- **A foreign orphan.** A `/usr/bin/grep` at 21–22 GiB ran at 100 % CPU for
  19–22 h with no parent (V). No layer below GitPulse catches that shape. The
  sensor now detects it (see Orphans).
- **The panics.** The two on 10-02 were launchd SIGBUS, probably the OS or
  hardware (I). The one on 10-07 was a watchdog timeout with the compressor
  segment limit at 100 % (I: memory thrash). The governor will not prevent the
  first kind.
- **Sessions were undercounted.** Insights counted 5 of 7 live Claude
  sessions. The cause (V, 2026-10-10): Claude Code writes its session registry
  under the session's own `CLAUDE_CONFIG_DIR`, and this host runs sessions
  under several. GitPulse read one directory.

## Ownership

| Trigger | Owner | Notes |
|---|---|---|
| One process over 32 GiB, or pressure level 4 for 3 samples | memguard | GitPulse never kills a single process for its footprint. |
| A `mac_heavy.sh` job tree over its RSS cap | mac_heavy | Card 4 exempts trees carrying `MAC_HEAVY_LABEL`. |
| The **sum** of a GitPulse-launched tree | Card 4 (supervisor) | Advise by default; enforce is opt-in. |
| Admission and shedding | Cards 1b and 1c, one verdict (`sysload::governor`) | See Admission and shedding. |
| Leases, the hook, advice | Cards 1d, 3 | **The sensor only measures and publishes.** They consume the same verdict. |

The sensor's one action is a person clicking **Terminate** on a foreign orphan
in Health. That signal is re-checked on the live process before it is sent
(see Orphans).

## The series

| Card | Scope | State |
|---|---|---|
| 1 (sensor) | This page: sampler, `host-load.v1`, sensor states, sessions by process scan, `GITPULSE_RUN_ID`, memguard health, orphans, Health | Implemented |
| 1b | Admission of agent launches on the sensor state | Implemented (see Admission and shedding) |
| 1c | Shedding GitPulse's own background and reactive work | Implemented (see Admission and shedding) |
| 1d | Leases and the priority queue ("time slots") | Not started |
| 3 | The `gitpulse-hook` check, the skill, `doctor` | Not started |
| Per-call gate cost | The hook's per-call budget | Separate card |
| Startup churn | The startup git-spawn burst | Separate card |
| 4 | Tree-sum supervisor and launched-orphan stop, advise by default | Not started |
| 5 | Disk-write budget; consumes the per-tree write rate | Not started |

## Sensor states

The contract cards 1b, 1d and 3 consume. A reader gets the state, the
**last-known level** and its age, whatever the state.

| State | Snapshot age |
|---|---|
| fresh | ≤ 10 s |
| stale-recent | ≤ 120 s |
| stale-old | ≤ 10 min |
| dead | older, never written, unreadable, written before this boot, or an unsupported platform |

The age comes from a monotonic clock shared by every process and counting
through sleep: `CLOCK_MONOTONIC` on macOS, `CLOCK_BOOTTIME` on Linux. It is
compared within one boot, identified by a hash of `kern.bootsessionuuid` (Linux:
`boot_id`). A wall clock stepped by NTP, a user or a VM resume can therefore
neither make a dead sampler look fresh nor a live one dead. `kern.boottime` is
deliberately not the boot identity, because macOS moves it when the wall clock
is stepped.

The ruling the consumers follow (Fable, round 3; A4/A5):

- **Agent launch:** fresh and stale-recent govern on the last-known level, with
  a "stale N s" warning. Stale-old and dead admit with a warning and a
  `sensor_unavailable` ledger event.
- **Heavy lane:** fresh and stale-recent govern. Stale-old refuses with
  `sensor_stale` and offers a recorded "admit anyway". Dead on a supported
  platform refuses with `sensor_unavailable` and offers an override. An
  unsupported platform gates on the lane only.
- **Hook:** fresh and stale-recent give a verdict. Anything else is advice
  only: "no sampler".

The frontend receives the same thing as `hostPressure` (`hostLoadStore.ts`).
It is `normal | warn | critical` while the sensor governs and is switched on;
otherwise it is `unavailable`, never a guessed `normal`.

## The snapshot, `host-load.v1`

A fixed-size record of 664 bytes in the per-user config directory
(`~/Library/Application Support/GitPulse/host-load.v1`), beside
`spawn-budget.v1`. It sits in the config directory rather than `TMPDIR` because
agent hosts give their children a private `TMPDIR`. It opens through the same
refusals as the spawn budget (`fs_entry::open_private_record`): it must not be
a symlink (`O_NOFOLLOW`), must be a regular file, owned by this user, mode
0600. A reader never creates it.

- **Header:** MAGIC `GPHL`, VERSION (1), length, flags, sequence.
- **Writer identity:** pid, process start time, binary path and version, so
  Health can say *which* GitPulse is sampling when a dev build and the
  installed app coexist.
- **`host_load.enabled`:** carried in the flags, so hooks never read
  settings and a flip reaches every reader on the next tick.
- **Readings:** each one has a status byte (a value, or *unsupported*,
  *unreadable*, *failed* or *pending*), so an unreadable reading is never
  decoded as zero.
- **Sampler observability:** ticks, overruns, missed ticks, last and slowest
  tick, and the sampler thread's own CPU time.

**Sequence lock.** The writer makes the head sequence odd, writes the body and
a tail copy of the next even sequence, then makes the head even. A reader reads
the record, then the head again. It accepts the record only when head, tail
and re-read head are one even number *and* the FNV-1a checksum over every
version-1 byte matches. Anything else is retried (bounded), then reported as
`Torn`, never decoded.

The checksum is insurance rather than a measured fix. The racing-reader test
accepted no mixed record with it disabled (0 of 12 runs). But the ordering a
cross-process sequence lock relies on, between two processes' `pread` and
`pwrite`, has no documented guarantee on APFS.

**Compatibility.** Fields are only ever added, at the end. A reader accepts
any `VERSION` at or above its minimum whose length covers every field it
knows. Anything else is a typed error (`UnsupportedVersion`, `Short`,
`BadMagic`, `Unsafe`, `Missing`, `Torn`). Tests decode a v2-shaped record with
a v1 reader, and a v1 record with a v2 reader.

**One writer.** Writing requires an exclusive advisory lock on
`host-load.v1.lock`, held for the writer's whole life. The per-profile
single-instance guard already makes the login-item launch and the foreground
app one process. The two-writer case is therefore a dev build or a second
profile beside the installed app: the loser reads the winner's snapshot and
retries the election every tick. A process forked by the writer holds a copy of
the lock descriptor until it execs. A writer that exits while such a fork is
in flight therefore keeps the lock a moment longer; the next tick retries.

## Where it runs

Only in the GitPulse app process. `lib.rs::run` starts the sampler from its
setup (`start_host_load_sampler`). `tests/host_load_sampler_scope.rs` pins that
this is the only start, and that no other shipped binary (`gitpulsed`,
`gitpulse-mcp`, `gitpulse-hook`) reaches it.

**Cadence.** Fixed at 2 s on a fixed grid. Hysteresis downstream counts
samples, so the interval never adapts. A tick that runs past the cadence is an
**overrun**. A grid slot the thread woke too late for is **missed**. Both are
published.

**Thread.** The sampler thread asks for the user-initiated QoS class and holds
an `NSProcessInfo` activity (`NSActivityUserInitiatedAllowingIdleSystemSleep`,
minus the sudden- and automatic-termination bits), so App Nap does not
coalesce its timer while the window is hidden. The activity never holds the
machine awake and never stands in the way of logout. Either request failing is
listed in Health's *problems*. Buffers (the process list, identity cache and
procargs buffer) are allocated once and reused.

## Readings

| Reading | macOS | Linux |
|---|---|---|
| Memory pressure | `kern.memorystatus_vm_pressure_level` (1/2/4) | PSI `memory`: `full avg10 ≥ 10` critical, `some avg10 ≥ 10` warn (**unverified**) |
| Swap used / total | `vm.swapusage` | `/proc/meminfo` |
| Compressor counters | `vm.compressor.swapper.wakeups.thrashing_detected` / `free_below_reserved`, `vm.compressor.swapper.swapouts_pressure`, `vm.compressor.compactor.major_compaction_segments_freed` | unsupported |
| Compressor bytes | `host_statistics64` `compressor_page_count` × page size | unsupported |
| Load 1/5/15 | `getloadavg` | `/proc/loadavg` |
| CPUs | `hw.logicalcpu`, `hw.perflevel0/1.logicalcpu`; no levels on Intel or Rosetta | logical count |
| Open files | `kern.num_files` / `kern.maxfiles` | `/proc/sys/fs/file-nr` |
| Free disk | `statfs` of the config directory's volume | `statvfs` |
| Thermal state | `NSProcessInfo.thermalState` | unsupported |
| Power source | `IOPSGetTimeRemainingEstimate` (−2 = external power) | unsupported |

No `compressor_bytes_used` or segment-limit sysctl exists. The counters above
are what there is.

**`compressor_thrash`** is raised when thrashing wakeups or pressure swapouts
rise in **each of three consecutive samples**. One quiet sample clears it. A
counter that goes down (a reboot, an overflow) rebases instead of producing a
delta. A gap in readability forgets the run, so three new rises must earn the
flag again.

**Apple silicon core levels.** On this host, `hw.perflevel0` is "Super" (6) and
`perflevel1` is "Performance" (12), with no efficiency level. "The P-core
count" is therefore not a number the sensor can honestly publish; it publishes
the levels as they are.

## Sessions by process scan

A Claude Code or Codex session is found by its **executable**, not by any
registry:

- **Claude:** an executable named `claude`, the native installer's
  `…/claude/versions/<version>`, or a Node or Bun runtime running
  `@anthropic-ai/claude-code`. Its short name is a version string
  (`2.1.296`), so matching on the name would miss every one.
- **Codex:** an executable named `codex` (`…/.codex/packages/standalone/…/bin/codex`),
  or Node running `@openai/codex`.

The path comes from `KERN_PROCARGS2`, which keeps the spelling the process was
exec'd with: a symlink named `claude` reads as `claude` there, where
`proc_pidpath` resolves it to its target. A process whose **direct parent** is
the same kind is a wrapper's target (Codex's npm shim starting the native
`codex`), not a second session. A Claude run from a shell under another session
is its own session.

**Launched vs observed.** Every PTY GitPulse starts carries
`GITPULSE_SESSION_ID`; a task attempt's also carries `GITPULSE_RUN_ID`.
`GITPULSE_RUN_ID` is the launch table's environment half
(`terminal_command::launch_env`, beside `policy()`'s flags). The PTY spawn path
sets it after the caller's own environment, so an IPC caller cannot file a
session under another run. It is *removed* from plain terminals, so a GitPulse
started inside an attempt does not hand that attempt's id to every shell tab.

A process belongs to the launched tree of its nearest ancestor-or-self carrying
either variable, read through `KERN_PROCARGS2`. A session is **launched** when
it is in such a tree and **observed** otherwise. `live_sessions` (the insights
facet and the MCP tools) counts from this scan. Claude Code's registry only
adds the entrypoint and last-reported status where an entry names the same
process (same pid, already running when the entry was written). On a platform
with no process scan (Windows), the registry is the detector again.

**Footprints and disk writes.** `ri_phys_footprint` (Linux: resident set) is
summed per launched tree and per observed session.
`ri_diskio_byteswritten` deltas give a per-tree write rate in bytes per second,
for card 5. Bytes written before the sampler first saw a process are a
baseline, not a rate.

## Limits

- **Apple platform binaries hide their environment.** On macOS,
  `KERN_PROCARGS2` returns no environment for Apple-signed system binaries
  (`/bin/sh`, `/bin/zsh`, `/usr/bin/grep`, `/bin/sleep`) (V). One of those that
  is still attached is attributed through the parent walk. One that is
  **reparented to launchd is lost to its tree.** Non-platform binaries (Claude,
  Node, Go, Cargo, Homebrew Python) carry the variable and stay attributed
  after reparenting (tested). Observed sessions' backgrounded children are lost
  the same way once they reparent.
- **Other users' processes cannot be measured.** `PROC_PIDTBSDINFO` refuses
  them (`EPERM`; 232 of 943 on this host). The short form still gives their
  owner, parent and name, so they are **counted** as *not inspected* rather
  than dropped. Their footprints are unreadable, not zero, so root processes
  never appear in the top five.
- **Pressure lags allocation.** Memory a VM or a browser takes shows in the
  pressure level about 6 s after it is allocated. The top-five footprints can
  move before the level does.
- **WebKit helpers appear foreign.** The window's `WebContent` process is
  parented to launchd, so it is listed among the non-GitPulse footprints.
- **Subagents run in-process.** A Claude Code subagent's tool calls run in the
  parent session's process tree; there is no second `claude` process (V,
  2026-10-10). Their work is attributed to the parent session.
- **Codex's live state.** Codex sessions are counted by executable, but
  nothing reports whether one is busy or idle.
- **Linux** footprints are resident set, not a compressed-memory-aware
  footprint, and the PSI mapping is provisional. On a Linux host with a
  subreaper (`systemd --user`, a container's init), an orphan is adopted by
  the subreaper rather than pid 1, so the orphan rule below (parent is pid 1)
  never lists it there.

## memguard

The sampler finds memguard by process name (`memguard`, wherever it is
installed; the path is reported, never hardcoded). It reports the pid, start
time and flags (`argv` through `KERN_PROCARGS2`). The log path comes from the
file behind its stdout or stderr (`proc_pidfdinfo`). The heartbeat age is the
log's modification time. Health warns on a governed platform when:

- memguard is not running;
- its log is outside `~/Library/Logs` (on 2026-10-09 it was writing to
  `~/.Trash/memguard/memguard.log`);
- its log path cannot be read; or
- it has not written for more than twice its `-heartbeat` interval.

GitPulse never starts, restarts or signals memguard.

## Orphans (advise only)

A foreign process is listed as an orphan when all of these hold:

- its parent is launchd;
- it belongs to this user;
- its working directory is in a git checkout;
- it has spent at least 80 % of its life on a CPU, over more than 30 minutes;
- it is in no session and no GitPulse launch, and is not memguard.

GitPulse launches that outlive their run are card 4's, not listed here. Health
lists each orphan with **Terminate**, which takes two clicks. The backend then
re-checks everything on the live process: same pid and start time, still this
user's, still parented by launchd, still no launch id or session, still
spinning in a checkout. It then asks in a native Yes/No dialog that names the
process (`desktop::confirm::native`), checks everything again once the dialog
closes, and sends SIGTERM only if all of that still holds. A No, a dialog the
host cannot draw, or anything changed refuses and signals nothing.

Security impact: `cmd_host_load_terminate_orphan` is granted to the main
window, so any script that reaches the IPC bridge can ask for it. The two
clicks are DOM and are not the boundary; the native dialog is, as for the
other grants that widen what GitPulse will do (repository trust, tool paths).
The signal is SIGTERM to a process this user already owns and could `kill`
from a shell; no access is widened beyond that. The other three host-load
grants (`cmd_host_load_status`, `cmd_host_load_settings`,
`cmd_host_load_settings_save`) read the sensor and toggle the sampler's kill
switch.

## Admission and shedding (cards 1b and 1c)

One function owns the decision: `sysload::governor::decide` turns a sensor
report into a **verdict** (admit or refuse launches, which spawn classes to
shed, and the reason). It is pure over a report, so it is tested with the
replay path (a real `host-load.v1` record read at chosen ages) and consumed
the same way by cards 3 and 4. The two callers are thin:

- **1b, `sysload::admit`.** Called before an agent launch's checkout probes
  (`terminal_launch::prepare_in`), before its capacity check and worktree
  build (`ensure_room`), and before an agent CLI's PTY opens
  (`terminal::spawn_session_inner`; the user's own shell is never gated).
  `tests/governor_sites.rs` pins those places by function name.
- **1c, `sysload::shed`.** Read by the spawn gate (`git_cli::acquire_until`)
  and nowhere else. A shed spawn is refused at once with the existing
  "deferred under load" marker, so the frontend's retries resume it when the
  pressure clears. There is no new scheduler: the classes are the ones
  `e9a64486` and `bb302b7f` introduced, and a user action (a commit, a save,
  or a read credited to one) is never shed.

| Input | Verdict |
|---|---|
| pressure `normal`, no thrash | nothing shed, launches admitted |
| pressure `warn` | background work shed, launches admitted |
| pressure `critical` **or** `compressor_thrash` | background and reactive work shed, launches refused |
| sensor stale-recent (at most 120 s) | govern on the last-known level, with a "stale Ns" warning |
| sensor stale-old or dead, level unreadable | admit with a warning, nothing shed; ledger `host_load.sensor_unavailable`; Health says "ungoverned" |
| `host_load.enabled` off, or a platform that is not governed | admit, nothing shed |

**What binds.** Two inputs, each named in the reason with its value:
the kernel pressure level, and the sensor's `compressor_thrash` flag (the
2026-10-07 panic was compressor exhaustion while the level lagged, so the level
alone would have admitted into it). The ruling left this as a user decision
(memory pressure only, or also the compressor); this build takes the second
option and puts it in one clearly named place, `Binding::CompressorThrash`.
Swap at or above a quarter of RAM, which the same ruling listed, is not an
input.

**Hysteresis.** A tier is entered after 3 consecutive sensor ticks at or above
it and left after 5 consecutive ticks below it, so a level flapping at a
threshold neither flaps the verdict nor escalates it. Ageing is measured on a
monotonic clock from the last tick, so a sampler that stops ticking walks the
verdict through stale-recent, stale-old and dead while it is asked.

**Overrides are human only.** Health's **Admit anyway** (and the same button
on a refused launch in the task form, beside **Retry**) asks in a native Yes/No
dialog that page script cannot click, then admits launches for 120 s
(`cmd_host_load_admit_anyway`, granted to the main window like the other
host-load commands; the dialog is the boundary, as for Terminate). There is no
agent `force`, and no queue: Retry asks again, a queue is card 1d's.

**A refusal never quietly becomes an allow.** After 30 minutes of continuous
pressure refusal the pulse carries a notice, and the app raises a toast that
names the binding resource and the largest foreign footprint, with a one-click
**Stop refusing launches** (`host_load.govern_launches` off). The verdict does
not change on its own at 30 minutes or at any later time. The notice is an
in-app toast, not a native banner.

**Never gated.** Stop, kill, cancel, save, commit, release and reconcile are
not admission sites. `cancel_release_and_reconcile_are_never_gated_by_pressure`
runs three of them at `critical`, and the spawn-gate test shows an interactive
spawn is admitted while background and reactive are shed.

**Ledger.** Each admission step records `host_load.admit`,
`host_load.refuse` (outcome `blocked`), `host_load.sensor_unavailable` or
`host_load.override_admit`, with the site, reason, binding, sensor state and
message, in the launch's repository. It is written off the launch path.

**Scope of shedding.** Only the app process sheds. `gitpulse-mcp`, `gitpulsed`
and the hook have no governor, so an agent's MCP calls are never refused on
pressure. Shedding acts on spawns through the gate; background work that
spawns no git child (a pure IPC read) is not shed by this card.

## Health and the status bar

Health → **Host load** (machine-wide, under every repository's Health) shows:

- memory pressure with the sensor state and age;
- the platform tier;
- which process writes the snapshot and from which binary;
- the readings;
- Claude and Codex sessions, launched and observed;
- launched trees with footprint and write rate;
- the five heaviest processes GitPulse did not start, with the 6 s lag note;
- the memguard line and its warnings;
- orphans;
- the governor: which classes are shed and why, whether launches are refused and for how long, **Admit anyway**, and "ungoverned" when the sensor is stale-old or dead;
- the sampler's own ticks, overruns, missed ticks, tick times and CPU.

It also says: *replay-verified; no real memory-pressure event has been
observed by this sensor yet*. That stays until the first natural pressure event
is recorded.

The status bar is quiet while the machine is fine. It speaks when pressure is
warn or critical, when the compressor is thrashing, or when the sampler has
stopped ("Load sensor: no sampler"). While the governor acts it adds "· launches refused" or "· shedding". Clicking it opens Health.

## Settings

**Settings → Agents → Host load → "Measure memory pressure and agent
sessions"** (`host_load.enabled` in `tools.json`). It is on by default and is
written to the file only when off. Off stops the readings, not the heartbeat:
the sampler keeps publishing a snapshot that says "switched off", so readers
can tell that apart from a sampler that died. A malformed `host_load` block
leaves the switch on and costs no other setting.

**Settings → Agents → Host load → "Refuse agent launches under severe memory
pressure"** (`host_load.govern_launches`, on by default, written only when
off). Off still sheds GitPulse's own work under pressure but never refuses a
launch; it is what the 30-minute notice flips.

## Part 0: what was settled before building

- **Characterization tests on the code as it stood** (commit `cd9dd718`):
  - `characterization_a_launch_is_admitted_on_the_session_count_alone`: no
    host-load input, so a launch is admitted at pressure 4 as at 1;
  - `characterization_a_terminal_claude_with_no_registry_entry_goes_uncounted`;
  - `characterization_sessions_registered_under_another_config_dir_go_uncounted`
    (5 of 7).
- **`host_statistics64` is reachable through the existing `libc`** (0.2.189:
  `host_statistics64`, `mach_host_self`, `vm_statistics64.compressor_page_count`).
  Verified by `real_host_readings_are_values_or_named_reasons`.
- **Subagents run in-process** (see Limits).

## Verification

Every result is tagged **real** (this machine's kernel and process table),
**simulated** (threads or fixtures standing in for processes) or **replayed**
(a recorded trace or snapshot read at chosen ages).

| What | How | Tag | Result |
|---|---|---|---|
| Four sensor states, last-known level with age | `replayed_ages_give_each_of_the_four_states_with_the_last_known_level` | replayed | pass |
| Wall-clock jump, previous boot, future-dated | `age_rides_the_monotonic_clock_through_a_wall_clock_jump`, `a_snapshot_from_a_previous_boot_or_the_future_has_no_age` | replayed | pass |
| `compressor_thrash` hysteresis | `replayed_thrash_needs_three_rising_samples_and_one_quiet_one_clears_it` | replayed | pass |
| Older and newer readers | `readers_accept_newer_additive_records_and_refuse_short_or_old_ones` | simulated | pass |
| Reader racing writer | `a_reader_racing_a_writer_sees_only_whole_records` | simulated | pass |
| Writer election, loser reads | `one_writer_per_directory_and_the_loser_reads`, `real_sampler_publishes_and_a_second_one_reads` | simulated / real | pass |
| GitPulse-launched terminal Claude counted | `a_terminal_claude_with_no_registry_entry_is_counted_as_launched`; fails on registry-only counting (0 vs 1) | real | pass |
| Reparented child attributed with its footprint | `real_reparented_fixture_is_attributed_to_its_run_with_its_footprint` (128 MiB fixture, under the 512 MiB ceiling) | real | pass |
| Other users' processes counted | `real_scan_counts_other_users_processes_instead_of_dropping_them`; fails on the drop-on-`EPERM` reader | real | pass |
| PID recycle | `a_recycled_pid_never_inherits_the_cached_identity`, `a_registry_entry_never_enriches_a_later_process_on_the_same_pid` | simulated | pass |
| `GITPULSE_RUN_ID` set, removed, unspoofable | `a_task_terminal_carries_its_run_id_and_a_plain_one_none_whatever_the_caller_sent` | real | pass |
| Sampler only in the app | `tests/host_load_sampler_scope.rs` | real (source) | pass |
| Sampler cost | `cargo bench --bench host_load` (results below) | real | recorded |
| 10-minute soak | `cargo test --release --test host_load_soak -- --ignored` (results below) | real (test process, not the app's hidden window) | recorded |
| 4-hour hidden-window soak | the installed app with its window hidden | real | **not run**: needs the installed app |
| Installed-app check | Health's sampler pid matches `pgrep`, snapshot age < 5 s, memguard green, "no sampler" when stopped | real | **not run**: needs the installed app |
| Real memory pressure | — | — | **not run**: no consented pressure stage; `memory_pressure -S -l critical` is forbidden while memguard is live, and no allocation over 512 MiB is made |

### Benchmark

`cargo bench --manifest-path src-tauri/Cargo.toml --bench host_load`, on
2026-10-10, on an 18-CPU, 64 GiB Apple-silicon Mac with about 25 agent sessions
running. Real: no fixtures, this machine's own table of **920 processes** (230
of them other users', counted but not inspected). Release build, 60 samples per
row after 5 warm-up runs (15 for the cold scan).

| Row | min | p50 | p95 | worst |
|---|---|---|---|---|
| scan, cold (every identity read; what a new `gitpulse-mcp` pays) | 6.70 ms | 9.81 ms | 10.33 ms | 13.36 ms |
| scan, warm (the sampler's steady state) | 1.81 ms | 2.44 ms | 3.40 ms | 3.96 ms |
| host probe | 56.9 µs | 88.7 µs | 707.5 µs | 1.82 ms |
| publish + read | 14.7 µs | 19.5 µs | 32.4 µs | 54.6 µs |

A steady-state tick (warm scan, probe and publish, p95 summed) costs about
4.1 ms of the 2 s cadence, roughly 0.2 % of one core. Compare builds by the
**min** column (min of N); the tail is what the cadence must absorb. The card
asked for a benchmark on about 1,000 processes; this table had 920, and the
row says how many it read rather than assuming.

### 10-minute soak

`cargo test --release --manifest-path src-tauri/Cargo.toml --test host_load_soak -- --ignored --nocapture`
on 2026-10-10 on the same machine, which had 1,361 processes by then. Real: the
production sampler on the real table at the production cadence, with its QoS
class and App Nap activity requested. It ran in a test process, **not** the app
with its window hidden; that condition is the 4-hour soak's, on the installed
app.

| Measure | Result |
|---|---|
| Duration | 600.0 s |
| Ticks | 300 of 300 expected |
| Missed ticks | 0 |
| Overruns (tick over 2 s) | 0 |
| Gap between ticks, p50 / p99 / max | 2000.1 / 2011.3 / 2015.5 ms |
| Tick time, last / slowest | 5.7 / 23.1 ms |
| Sampler CPU | 1.55 s in 600 s (0.26 % of one core) |
| Problems reported | none |

`GITPULSE_HOST_LOAD_SOAK_SECS=14400` runs the same test for four hours, and
`GITPULSE_HOST_LOAD_SOAK_REPORT=<file>` writes the JSON above to a file. The
recorded 4-hour hidden-window soak is still to be run against the installed
app, as is the installed-app check in the table above.
