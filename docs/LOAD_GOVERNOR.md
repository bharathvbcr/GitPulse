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
| 3 | The `gitpulse-hook` check, the skill, `doctor` | The command-gate verdict, the session-brief line, the skill and the plugin-cache drift check are implemented (see The command gate). Leases, `gitpulse_system_load`, launch-environment caps and `CLAUDE_ENV_FILE` caps are not |
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

A fixed-size record of 712 bytes (version 2: the 664 bytes of version 1, then a
48-byte extension; see Compatibility) in the per-user config directory
(`~/Library/Application Support/GitPulse/host-load.v1`), beside
`spawn-budget.v2`. It sits in the config directory rather than `TMPDIR` because
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
- **Extension (version 2, bytes 664 to 712):** the compressor's segment
  readings (`total` and `limit`, from `vm.compressor.segment.total` and
  `.limit`, which read as plain integers in-process; each with a status byte
  like every other reading) and **the governor's published verdict**: tier,
  binding, whether launches are admitted, whether background work is shed and
  whether reactive work is shed. It is the hysteresis-held verdict of the app's
  sampler, so a reader with no history (the hook, `gitpulse-mcp`) can use it
  without reading settings. **It is one tick behind the readings beside it**:
  the sampler publishes, then counts the sample, so a record carries the
  verdict the previous count produced (pending on a writer's first tick, and
  unsupported while the sensor is switched off). The hook reads it (see The command
  gate). The extension opens with the magic `GPX2` and ends
  with a checksum of its own, which also covers the sequence.

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

Version 2 appends after byte 664 and leaves every version-1 byte, the tail
sequence at 656 and the version-1 checksum formula where they were, so:

- **An older build reading this build's record** accepts it (version 2 is at or
  above its minimum 1, the length covers every field it knows, the tail and
  checksum are where it looks) and reads the same readings it always did. It
  never sees the extension.
- **This build reading an older build's 664-byte record** also accepts it. The
  extension is optional here: its fields read as unsupported, never as zero. (A
  reader that *required* the new length would have read an older writer's record
  as `Short`, so the sensor would read dead and nothing would be governed while
  an older build still writes; this build does not do that.)
- A record that does not open its extension with `GPX2` (a longer record from
  some other newer writer) reads the same way. One that does but fails the
  extension's checksum is `Torn`, never decoded.
- A writer that wins the election sizes the file to its own length, so an older
  writer's shorter record is grown (zero-filled until the first publish, which
  readers see as "not yet written") and a newer one's longer record is cut.

**One writer.** Writing requires an exclusive advisory lock on
`host-load.v1.lock`, held for the writer's whole life. The per-profile
single-instance guard already makes the login-item launch and the foreground
app one process. The two-writer case is therefore a dev build or a second
profile beside the installed app: the loser reads the winner's snapshot and
retries the election every tick. A process forked by the writer holds a copy of
the lock descriptor until it execs. A writer that exits while such a fork is
in flight therefore keeps the lock a moment longer; the next tick retries.

## The trace, `host-load-trace.v1`

`host-load.v1` is one overwritten record, so after the 2026-11-14 hard reset
nothing said what the last minutes looked like. The trace is a bounded ring
beside it, in the same directory, that keeps them.

- **Layout.** A 64-byte header (magic `GPHT`, version, record length 128,
  capacity, the boot id it belongs to, a checksum) then 1,080 fixed 128-byte
  slots. Each record carries its sequence number and its own checksum; the slot
  is `(seq - 1) % capacity`, so the file never grows past 138,304 bytes and the
  oldest record is overwritten first.
- **Cadence.** One record every 5th sampler tick (10 s at the 2 s cadence), so
  the ring holds three hours.
- **Who writes.** Only the elected writer sampler feeds the ring; a reader
  process never opens the file for writing. `tests/host_load_sampler_scope.rs`
  pins that `TraceFeed` is named only by the sampler and `trace.rs`, and
  `TraceWriter` only by `trace.rs`. The file is opened through
  `fs_entry::open_private_record`, so a symlink, a file owned by someone else
  or one readable by others is refused and never touched.
- **Off the tick.** The sampler tick only offers a record to a one-slot
  hand-off (a lock held for nothing but moving the record) and never waits on
  the file. A thread of its own, `host-load-trace`, started through
  `supervisor::spawn_once` and only by the elected writer, opens the ring (so
  the rotation of the previous boot's ring is off the tick as well), then
  writes and flushes each record. If the slot is still full at the next offer
  the new record is dropped and counted, and the count rides in the next record
  that lands (`dropped_before`, and `dropped_total` since the feed started,
  stored in bytes 104..120 that were spare, so a record from before this
  existed reads as nothing dropped, an older build still validates a newer
  record, and the layout version stays 1). While drops are happening the
  status carries a problem line (for 60 s after the last), and the previous
  boot's Health block says how many readings its ring lost. `stop` waits at
  most 2 s for the thread; a flush blocked in the kernel cannot be
  interrupted, so past that the thread is detached. It owns the file and its
  own state, finishes that write and exits, and nothing the sampler holds
  depends on it.
- **Fields.** Wall seconds, monotonic ms, boot id, memory pressure, the
  governor's held tier, binding and admit verdict, swap used, compressed bytes,
  1-minute load, free disk, launched and observed session counts, the largest
  foreign footprint, the summed launched footprint, thermal state, power
  source and the compressor segment fraction. A reading that could not be taken
  is stored as unreadable (a sentinel no real value can take) and shown as
  unreadable, never as zero.
- **Cost and durability.** One 128-byte write and one `File::sync_all` per
  10 s. On Apple targets the standard library implements `sync_all` (and
  `sync_data`) as `fcntl(F_FULLFSYNC)`, a whole-device cache flush (verified in
  `library/std/src/sys/fs/unix.rs` at the installed rustc's commit
  0ef1bc386). So a hard reset loses the record being appended (up to 10 s of
  readings) and any record the sampler dropped while a flush was stuck, not
  what the drive had in its cache. The price, measured on the owner's Mac at
  load average about 20 (300 appends, interleaved with plain `fsync(2)`):
  `sync_all` min 2.9 ms, median 4.9 ms, p95 11.2 ms, max 23 ms; plain
  `fsync(2)` median 0.07 ms, max 3.7 ms. None of it is on the tick any more, so
  a stalled flush no longer delays the `host-load.v1` publish or the governor's
  count (tests: the sampler keeps publishing with the write blocked, and with
  the open blocked). A torn record fails its checksum and is dropped on read.
  Drops after the last record that reached the ring leave nothing to carry
  their count: a stall that runs into the reset shows as a trace that ends
  early, and the "ended" time is the last record's, not the reset's.
- **Boot change.** When the trace thread opens the ring and the header's boot id differs
  from this boot's, the file is renamed to `host-load-trace.prev.v1` (replacing
  any older one) and a fresh ring starts. A restart inside the same boot resumes
  the sequence. A short, torn or foreign file is a typed error, is never
  decoded, and is replaced.
- **Where it shows.** `HostLoadStatus.previous_boot` carries the last 30
  records of the previous boot plus the peaks over them (or the typed error).
  Health, Host load, renders it as "Before the last restart". No new IPC
  command was added; the status payload grew.

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
| Compressor segments | `vm.compressor.segment.total` and `vm.compressor.segment.limit` | unsupported |
| Load 1/5/15 | `getloadavg` | `/proc/loadavg` |
| CPUs | `hw.logicalcpu`, `hw.perflevel0/1.logicalcpu`; no levels on Intel or Rosetta | logical count |
| Open files | `kern.num_files` / `kern.maxfiles` | `/proc/sys/fs/file-nr` |
| Free disk | `statfs` of the config directory's volume | `statvfs` |
| Thermal state | `NSProcessInfo.thermalState` | unsupported |
| Power source | `IOPSGetTimeRemainingEstimate` (−2 = external power) | unsupported |

No `compressor_bytes_used` sysctl exists, and the counters above are the
cumulative ones. This document used to say a segment-limit sysctl did not
exist either; that was wrong. Verified on 2026-11-14 on the 64 GiB host that
crashed that day (`sysctl vm.compressor.segment.total
vm.compressor.segment.limit`): they print `256816` and `1677721`, and later
`254515` and `1677721`, so the compressor was using about 15 % of its segment
limit when read. Both are plain integers (`sysctl_uint` reads them whether
declared 32 or 64 bits wide). The governor's `compressor_segments` input is
`total / limit`; see What binds.

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
| any input below at its elevated threshold, in mode `shed` or `refuse` | background work shed, launches admitted |
| any input below at its severe threshold, in mode `refuse` | background and reactive work shed, launches refused |
| an input in mode `off`, or one whose reading was never provided | ignored, no warning |
| an input in mode `shed` or `refuse` whose reading could not be taken | no tier, and the verdict carries "swap is unreadable: not governed on" |
| sensor stale-recent (at most 120 s) | govern on the last-known level, with a "stale Ns" warning |
| sensor stale-old or dead, level unreadable | admit with a warning, nothing shed; ledger `host_load.sensor_unavailable`; Health says "ungoverned" |
| `host_load.enabled` off, or a platform that is not governed | admit, nothing shed |

**What binds.** Eight inputs, each named in the reason with its value and the
threshold it crossed. The first two have no setting:

1. the kernel pressure level (`memory_pressure`);
2. the sensor's `compressor_thrash` flag (the 2026-10-07 panic was compressor
   exhaustion while the level lagged, so the level alone would have admitted
   into it).

The other six were added on 2026-11-14, after the owner's Mac (64 GiB, 18
logical CPUs) hung and reset with swap used at 0.9 times physical memory, a
1-minute load of 120 (6.7 per CPU) and about 32 Claude processes, while the
kernel level read only `warn` and the governor said "launches admitted". The
sensor already published every one of those readings and nothing decided on
them. Each has a mode and two thresholds, changeable in Settings. **The
defaults are provisional: they were set from that one incident.**

| Input (`Binding`) | Measures | Default mode | Elevated | Severe |
|---|---|---|---|---|
| `compressor_segments` | `vm.compressor.segment.total / vm.compressor.segment.limit` | refuse | 0.60 | 0.80 |
| `swap` | swap USED / PHYSICAL memory (never swap total: macOS grows swap on demand, so the total reads 0 after boot) | refuse | 0.50x | 0.75x |
| `low_disk` | free space on the volume holding GitPulse's state; lower is worse | refuse | below 20 GiB | below 10 GiB |
| `thermal` | `NSProcessInfo.thermalState` | shed | serious | critical (refuse mode only) |
| `cpu_load` | 1-minute load average / logical CPUs | shed | 3.0 | 6.0 |
| `battery` | running on battery | off | on battery | never |

Modes: **off** ignores the input; **shed** lets it raise the tier to Elevated at
most (GitPulse slows its own background work, launches are admitted);
**refuse** lets it reach Severe too (reactive work is shed and launches are
refused). Refusal is still subject to `host_load.govern_launches`: with that
off nothing ever refuses.

The verdict takes the highest tier any input reaches. On equal tiers the
binding is the first of: memory pressure, compressor thrash, compressor
segments, swap, low disk, thermal, CPU load, battery. The sentence names the
resource, its value and the threshold, for example "swap used is 0.83x
physical memory (53.4 GiB of 64.0 GiB), at or above 0.75x". While the
hysteresis still holds a tier the reading has already left, it says "now back
under 0.75x" instead of claiming a crossed threshold.

An input that is on and could not be read contributes no tier and adds one
warning ("swap is unreadable: not governed on"); a reading the platform never
provides (unsupported, or not published by an older writer) is not a failed
check and adds no warning. A zero or unreadable denominator (physical memory,
CPU count, segment limit) makes the input unreadable, never infinite or zero.

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

## The command gate

On 2026-11-14 this Mac hung with swap at 0.9x RAM and a load of 120 on 18
cores, under sessions GitPulse only observes, so launch admission never touched
them. The only GitPulse code that runs inside those sessions is the plugin's
`PreToolUse` `command-gate` hook, so the hook reads the app's published verdict
(`PublishedVerdict`, above) and acts on it. It never samples the host and never
re-derives a threshold: it refuses what the governor refuses to admit.

The load stage (`hooks/load_gate.rs`, the table in `hooks/heavy_commands.rs`)
runs **before** the harness is asked, and is folded in after the guard and the
gate are combined, so the harness's 5 s budget expiring cannot drop it.

| The command | The verdict | The hook |
|---|---|---|
| not in the table | any | nothing, zero added output, no snapshot or settings read |
| heavy | not admitted; sensor fresh or stale-recent | `enforce`: **deny** with a reason; `advise`: the same text as context |
| heavy | elevated, admitted | one line of advice, with the capped form |
| heavy | clear | nothing |
| heavy | no verdict: sensor stale-old or dead, no snapshot, a record with no verdict (an older GitPulse), measurement switched off | one line: **not** load-checked, and why, said once per session per distinct reason (an exclusive `loadnote-<session>-<reason>` marker in the same private directory as the loop breaker's; the line says it will not repeat). With no session id, or a directory that cannot keep the marker, it is said every time, never dropped. Never a refusal |
| heavy, but the line could not be read | | the same "not load-checked" line, once per session |
| `grep -r` with `-z`/`-a`/`--null-data` or no `--exclude-dir`; `rg -uu`/`--no-ignore` with no `-g '!…'`; `go test ./...` or `-fuzz` with no `-timeout`; `python` with `train` or `bench` in its argv | elevated or severe | one line of advice. Never a refusal; silent on a quiet host |

Heavy is a table (`ROWS`), not branching: `cargo` build/b/check/c/clippy/test/t/
bench/nextest/llvm-cov/doc, `go` build/test, `npm`/`bun`/`pnpm` `test` and `run`
of `test*` or `ci:local`, `vitest`, `jest` (also through `npx`, `pnpm exec`,
`node …/vitest/…`). It reads the command with the signal guard's own lexer
(`signal_guard::command_words`), so `VAR=val`, `env`, `time`, `nice`, `cd x &&`,
`;`, `||`, pipes, `$( )`, backticks and `bash -c '…'` are seen through, and a
word in a quoted string (`echo "cargo test"`, `git commit -m "run cargo test"`)
is not a program. A script's name is all it reads: `package.json` is not
resolved, so a script called `check` that runs the whole suite is not heavy
here (recorded follow-up). The heavy-lane holder is exempt: `MAC_HEAVY_LABEL` in
the hook's environment, or a command run through `mac_heavy.sh`.

It never emits `allow`, `ask` or `updatedInput`: an unearned allow would
override the person's permission rules.

**Bypass mode does not waive it.** `under_bypass` turns the guard's and the
gate's refusals into notices because they are permission questions and bypass is
the host's "stop asking me". This one protects the machine from every session at
once, and the sessions that hung it run in bypass, so it is enforced there. A
person turns it down in Settings. That the host honours a hook `deny` while in
`bypassPermissions` is **not** stated by the hooks documentation fetched below;
it is inferred from `under_bypass` existing and was not run against a live
session.

**The loop breaker.** The fifth refusal of the same command (compared trimmed)
in one session inside ten minutes says "STOP: you are looping", tells the person
through the user-facing message, and grants nothing. State is one marker file per
refused tool call (`loopdeny-<session>-<digest>-<tool_use_id>`), created
exclusively in the advisory-dedupe directory (private to the user, symlink and
foreign-owner refusal, bounded scan, 15-minute prune), so two copies of the hook
answering one call count once and nothing in a marker can be corrupt. A call with
no session id or no tool-use id (an orchestrated child terminal) is refused
plainly and never counted. A directory that cannot be used is a plain refusal,
never an allow.

**The setting** (Settings → Agents → Host load → "Command gate"): `enforce`
(the default; "Refuse" in the UI), `advise`, `off`. Stored as its own top-level
`host_load_hook_mode` key, written only when it is not `enforce`. Not inside
`host_load`, whose strict reader would make an older build discard the whole
block (the sensor switch with it), and not inside `host_load_thresholds`, which
an older build rewrites from the keys it knows. An older build keeps a top-level
key it does not know. The hook reads it from `tools.json` itself
(`tool_config::host_load_hook_mode`), only once a command is in the table; an
unreadable file or an unknown value enforces.

**Docs the fields come from** (fetched 2026-11-14): `https://code.claude.com/docs/en/hooks`
(`PreToolUse` `hookSpecificOutput`: `hookEventName` required, `permissionDecision`
of `allow`/`deny`/`ask`/`defer`, `permissionDecisionReason` shown to Claude,
`updatedInput`, `additionalContext`; common output `systemMessage`; text capped
at 10,000 characters; input `session_id`, `permission_mode`). Not confirmed from
those pages, though the code reads it already: `tool_use_id` on the input.

**`mcp:doctor` and the plugin cache.** The doctor (`scripts/check-mcp-install.mjs`)
also compares the `hooks/hooks.json` the host has installed (found through
`~/.claude/plugins/installed_plugins.json`, or `$CLAUDE_CONFIG_DIR`, or
`--claude-dir`) with the one this tree ships, and names the events missing from
the cache, declared differently, or only installed. A hook missing from the
cache never runs in a session that loads the plugin from it, and nothing else
says so; on 2026-11-14 the cache at 1.4.0 lacked eight events the repository
declares. The verdicts are `ok`, `drift` and `unverifiable` (both fail), and
`absent` (the plugin is not installed from the cache: reported as a note, not a
pass). It only reads; refreshing the cache is the host's plugin update.

**The session brief** (`session-brief`, SessionStart) carries one `host load:`
line right under its header, where the brief's 2,000-byte cap never cuts: tier and
what binds it with the sensor's state, or `UNKNOWN (why)` and that heavy commands
are not load-checked, and the Claude and Codex sessions launched and observed. It
reads the same snapshot and never samples. The `gitpulse-load` skill
(`plugins/gitpulse/skills/gitpulse-load`) says what to do with it and with a
refusal; the terminal launch prompt names it, because `dc-store` owns the shared
brief and does not.

**Cost**, in process (a debug build, so an upper bound), 9 rounds of 200 calls per
arm, interleaved and rotated, the median of round medians (the range of the
medians in brackets), run twice with the host load average at 11 and then 15 on
18 cores (`cargo test --lib -- --ignored --nocapture load_stage_cost`). A command
that is not heavy: 1.7 µs [1.6 to 1.9]. One of 10 KB: 175 to 179 µs. A heavy
command with a clear host, settings cold and the snapshot read from disk: 43 µs
at load 11 and 61 µs at load 15 [40 to 75]. A refused one, which also writes the
loop marker: 464 and 746 µs [427 to 879]. The session brief's added line: 25 µs
[17 to 34]. The hook process's own start (about 25 ms, `tests/hook_gate_bench.rs`)
is the cost to look at; this stage is not measurable against it, and no
end-to-end speed claim is made. The session brief's end-to-end time before and
after was **not** measured: that needs a second, pre-change hook binary.

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

**Settings → Agents → Host load → "Refuse agent launches when the machine is
overloaded"** (`host_load.govern_launches`, on by default, written only when
off). Off still sheds GitPulse's own work under load but never refuses a
launch; it is what the 30-minute notice flips.

**Settings → Agents → Host load inputs.** One row per input (swap, memory
compressor, CPU load, heat, battery power, free disk space), each its own
Settings search entry: a mode ("Off", "Slow GitPulse's background work", "Also
refuse agent launches"), its two thresholds with units, the live reading beside
it ("now 0.12x"), and a per-row reset. "Reset to defaults" in the Host load card
resets them all. A value that fails validation is refused with the rule it
broke and nothing is saved.

Storage: the thresholds are **not** inside `host_load`. They are a top-level
`host_load_thresholds` key in `tools.json`, holding only the inputs that differ
from their default (so a later build that changes a default reaches a person who
never chose a value), written only when something differs. Reading is lenient
per input: a value that cannot be read or fails validation falls back to that
input's own default with a warning, and a block that is not an object is all
defaults; neither costs any other setting. Why not inside `host_load`: that
block refuses unknown fields and is read leniently, so an older build that met a
`thresholds` field in it would discard the whole block, `enabled: false`
included, and its next save would delete the field. A top-level key lands in the
older build's `unknown` map, which it writes back as it read it. What an older
build does with a newer `tools.json`: ignores `host_load_thresholds`, loses no
other setting, keeps the key across its own saves, and governs on the old two
inputs only.

## Disk: two owner decisions

2026-10-21: low disk is advice, never a refusal. It can skip the optional
build-cache copy of a worktree (`LOW_DISK_FLOOR`, `Advisory::LowDisk`), which is
unchanged.

2026-11-14: the governor's guards become configurable with protective defaults.
Free disk is one of them (`low_disk`): with mode `refuse` (the default) and free
space below the severe threshold (10 GiB) a launch is refused with that binding;
with `off` or `shed` no launch is ever refused on disk, which is the 2026-10-21
behaviour. The test that pinned the old rule is rewritten to pin both
(`governor::tests::low_disk_advice_never_refuses_and_the_disk_setting_does_only_in_refuse_mode`).

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

### Hook session scan

`gitpulse-hook` is a new process on every call, so the in-process cold row
above is not what the collision guard pays. `observe_sessions` — the read
that guard makes on a file write — used to run the full table scan. It now
lists pids and reads arguments only for a process whose short name could be
an agent, or whose name is empty or a version string and whose resolved
executable could. Footprints and disk writes stay on the sampler's scan.

Measured on 2026-10-14 in fresh release processes, 15 samples each
(`GITPULSE_HOST_LOAD_ONCE` on the `host_load` bench). The before table had
1,247–1,250 processes and 33 sessions. The after table had 1,197–1,205
processes and 35 sessions. Compare the **min**.

| Row | min | p50 | p95 | worst |
|---|---|---|---|---|
| before, full scan (what `observe_sessions` paid) | 20.34 ms | 22.95 ms | 29.61 ms | 29.82 ms |
| after, `observe_sessions` | 3.76 ms | 5.84 ms | 14.02 ms | 15.12 ms |
| after, full scan (the sampler's path, left as it was) | 17.70 ms | 21.42 ms | 33.82 ms | 34.85 ms |

`collision_risk_for_path` on this checkout, same fresh processes, still
spends most of its time listing git worktrees. Before: min 377.5 ms, p50
646.0 ms, p95 969.0 ms, worst 1005.2 ms, 12 worktrees. After: min 287.1 ms,
p50 340.6 ms, p95 563.7 ms, worst 1049.4 ms, 11 worktrees. `sessions_ok` was
true on every sample. A scan that cannot list the table is still an error,
not an empty session list.

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
