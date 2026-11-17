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
| 1d | Leases and the priority queue ("time slots"), and idle-session advice and parking | Implemented (see Build leases and idle sessions). The shim cutover is the owner's |
| 3 | The `gitpulse-hook` check, the skill, `doctor` | The command-gate verdict, the session-brief line, the skill and the plugin-cache drift check are implemented (see The command gate). Leases are card 1d's. `gitpulse_system_load` and launch-environment caps are not |
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
  gate). The extension opens with the magic `GPX3` (`GPX2` was the block before the two disk fill rates changed its layout; a branch build that reads `GPX2` ignores this one) and ends
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
- A record that does not open its extension with `GPX3` (a longer record from
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
- **1c, `sysload::shed`.** Read by the spawn gate (`process::gate::acquire_until`)
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
defaults are provisional: they were set from that one incident.** The disk
floors are not fixed numbers: swap files share the volume, so unless a person
sets them they follow this computer's memory (see Disk guards).

| Input (`Binding`) | Measures | Default mode | Elevated | Severe |
|---|---|---|---|---|
| `compressor_segments` | `vm.compressor.segment.total / vm.compressor.segment.limit` | refuse | 0.60 | 0.80 |
| `swap` | swap USED / PHYSICAL memory (never swap total: macOS grows swap on demand, so the total reads 0 after boot) | refuse | 0.50x | 0.75x |
| `low_disk` | free space on the volume holding GitPulse's state; lower is worse | refuse | below the larger of 20 GiB and half of physical memory (32 GiB on a 64 GiB Mac) | below the larger of 10 GiB and a quarter of it (16 GiB on a 64 GiB Mac) |
| `disk_filling` | how fast that free space is falling, as minutes until the severe floor (see Disk guards) | refuse | under 30 minutes | under 10 minutes |
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
segments, swap, low disk, disk filling, thermal, CPU load, battery. The sentence names the
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

**Bypass mode does not waive it, and that is the one exception to
`under_bypass`.** `under_bypass` (`hooks/mod.rs`) turns the guard's and the
gate's refusals into notices because they are permission questions, and bypass
is the host's "stop asking me". A load refusal is not one: it protects the
machine from every session at once, not a permission the person waived, and the
sessions that hung this Mac on 2026-11-14 ran in bypass mode, so waiving it
there would leave this feature advice-only for the sessions it exists for. The
comment on `under_bypass` states the same exception. A person turns it down in
**Settings → Agents → Host load → Command gate**: Advise gives the model the same
text and refuses nothing, Off says nothing about load.

The host enforces a hook `deny` in `bypassPermissions` mode. Confirmed by
`https://code.claude.com/docs/en/agent-sdk/permissions` (fetched 2026-11-15):
its numbered evaluation flow lists "1. Hooks" ahead of "4. Permission mode", and
it says "A hook deny applies even in `bypassPermissions` mode." (The same page
also says hooks "still execute and can block operations if needed" there.) That
is the Agent SDK page; the hooks and permission-modes pages read the same day
did not state it. This is documentation only: it was not exercised against a
live bypass session.

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
at 10,000 characters; input `session_id`, `permission_mode`). `tool_use_id` on the input is confirmed, fetched 2026-11-15 from the same page
(PreToolUse input): "In addition to the common input fields, PreToolUse hooks
receive `tool_name`, `tool_input`, and `tool_use_id`." 

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

## Build leases and idle sessions (card 1d)

### Why

On 2026-10-17 four to seven release builds from separate agent worktrees
starved the window server. On 2026-11-14 sixteen runs queued for two slots of
a shell shim while the host was healthy. The shim (`~/.claude-bin/cargo`) sees
neither the session a build belongs to, nor the governor's verdict, nor who is
waiting. `sysload::lease` owns that state and `gitpulse-hook` is the program a
build goes through.

### The lease

A heavy `cargo` or `go` command takes one of N slots and holds it until the
build's own process exits. State is under `<config>/GitPulse/leases/`:

- `slot-1` to `slot-16`: lock files. The holder takes a POSIX record lock
  (`fcntl F_SETLK`) on its slot and then `exec`s the build, so the build
  process owns the lock. A record lock is not inherited by a child, so a
  daemon a build leaves behind cannot keep the slot, and the kernel frees it
  when the process dies, crashes included. There are 16 files for at most 8
  slots so that a build that overflowed still has a file to hold.
- `slot-N.json`: display only (tmp then rename). **A record without a lock is
  never a holder; a lock without a record is a holder shown as "pid X,
  unknown".**
- `queue/<mono_ns>-<pid>.json`: a waiter's ticket, locked by the waiter. A
  ticket nobody locks is dead and any reader deletes it.
- `paths/leasepath-<session>` and `leasemiss-*` markers (see below).

Service order is `(priority, slots the session already holds, enqueue time,
pid)`, with priority GitPulse terminal, then task run, then observed session.
A waiter takes a free slot only when its rank is below the number of free
slots. The board reads the queue before the slots and a grant re-counts after
it takes a file, releasing if it went over, so a race can only under-grant.

A process that holds a lock loses it by closing *any* descriptor for that
file. The code never reopens its own slot or ticket, and tests that need a held
slot hold it from another process.

### How many slots

`host_load_leases.slots` (default 4, 1 to 8), then the governor's published
verdict:

| Verdict | Slots |
|---|---|
| Clear | the setting |
| Elevated | `max(1, setting / 2)` |
| Severe, or launches not admitted | none: nothing new is granted |
| Not readable | the setting, and the wrapper says "slots not lowered for load: <why>" |

A build that waited `wait_max_s` (default 1800) runs anyway ("overflow"),
unless the verdict refuses, in which case it keeps waiting until `wait_hard_s`
(default 14400) and then exits 75 with nothing built.

### The wrapper

`gitpulse-hook` run as `cargo` or `go` (symlinks the app keeps in
`<config>/GitPulse/lease-bin`, maintained by `sysload::lease::ensure_links`),
or `gitpulse-hook lease -- <program> ...`. It runs before the hook's own start
deadline. It leases a `cargo` verb that compiles or tests (build, check,
clippy, test, bench, doc, rustc, rustdoc, fix, install, nextest, llvm-cov,
mutants, fuzz, miri, and the short aliases) and `go build` and `go test`. It
never leases `cargo run`, `clean`, `fmt`, `metadata`, `tree` or `help`,
`cargo llvm-cov clean`, `report` or `show-env` (the owner's shim lets these
through too), a call that only asks for `--help` or `-h` before any `--`, `go
run`, `env`, `mod` or `version`, a nested call (`GITPULSE_LEASE_HELD`, `CARGO_SLOT_HELD`), a `mac_heavy` job
(`MAC_HEAVY_LABEL`), or anything with `host_load_leases.mode` off.

A leased build gets `GITPULSE_LEASE_HELD=<slot>`, `CARGO_SLOT_HELD=gitpulse-lease`
(so the owner's shim does not queue it a second time), and, unless the caller
set one, `CARGO_BUILD_JOBS=max(2, cpus / slots)`. Incremental compilation stays
on. `host_load_leases.incremental_off` (default false) sets `CARGO_INCREMENTAL=0`
for leased builds; nothing is deleted.

**A build the wrapper cannot lease still runs, and says so.** A lease directory
it cannot use, or a verdict it cannot read, prints "did NOT lease this build:
<why>" and writes a `leasemiss-` marker; Health counts markers from the last 15
minutes (a read looks at a bounded number of entries, and when the folder holds
more, Health says "At least N" and never "No build"). Health also counts running
`cargo` and `go` processes that hold no slot ("N cargo processes hold no slot"),
or says it could not count them. A wrapper that is still waiting for a slot, a
build's own children, and a proxy's child (rustup's `cargo`) are not counted.

### How a session reaches the wrapper

- GitPulse terminal and task-run PTYs for an agent launcher get `lease-bin`
  first on `PATH` and `GITPULSE_LEASE_BIN` (`sysload::lease::launch_env`), and
  only when the `cargo` and `go` links resolve to a file.
- Every Claude session, launched or observed, gets `export PATH="<lease-bin>:$PATH"`
  appended once to `$CLAUDE_ENV_FILE` by the SessionStart hook (a repeat start
  does not stack copies), and a `leasepath-<session>` record is kept. The launch
  environment alone is not enough: a shell startup file such as `~/.zshenv`
  can put another `cargo` ahead of it for every `zsh`, `zsh -c` included, and
  only the env-file line puts the folder back before each Bash command.
- **A session is called gated only on that record.** `GITPULSE_LEASE_BIN` is
  not evidence. Without the record, the session brief says "NOT gating this
  session's builds: <why>" and the command gate says once per session "did NOT
  lease-gate this build". When every slot is held it says "Every build slot is
  held (...)", and when load refuses new builds it says so and how long the
  wrapper may wait; neither promises a Bash timeout.

Session key: `terminal:<GITPULSE_TERMINAL_SESSION>`, `run:<GITPULSE_RUN_ID>`,
`agent:<pid>-<start>` (the nearest agent ancestor), or `process:<ppid>-<start>`.

### Health, Settings, the terminal pill

Health, under Host load: **Build slots** (the effective count and why, holders
with age, the queue, builds that ran unleased, running builds that hold no
slot, a missing wrapper link) and **Idle sessions**. Settings, under Host load:
Queue or Off, the slot count, and "Turn incremental compilation off for leased
builds". The terminal pill's menu names a session in the queue ("Waiting for a
build slot (2nd)"); when the queue cannot be read it says nothing rather than
guess.

Settings are stored as the top-level `host_load_leases` and `host_load_idle`
keys of `tools.json`, only where a field differs from its default, read
leniently per field (an invalid block falls back to defaults and costs no
other setting).

### Idle sessions and Park

A session is **idle** only when all of these hold; any one that cannot be read
makes it **not known**, never idle:

1. the agent reported that it is waiting (a standing hook event, Ask or
   Finished) for at least `host_load_idle.idle_after_s` (default 900);
2. its whole process tree used under 5% of one core over the last tick (a
   first tick, or an unreadable member, is unknown);
3. where GitPulse holds the PTY, the terminal has been quiet for the same time;
4. it holds no build slot and is not queued.

A PTY that is quiet is not enough, and neither is a low CPU reading: an agent
that is thinking silently reports no waiting state. Codex, observed sessions
and sessions with no hook reports are always "not known".

**Park** closes a GitPulse-launched Claude session's terminal and keeps its
conversation so the existing reopen offer ("Resume") brings it back. It is not
SIGSTOP. It is offered, and enforced by the backend, once a session has been
idle for `host_load_idle.park_after_s` (default 3600). Only a terminal that
GitPulse launched as that agent can be parked: a plain shell tab with `claude`
typed into it carries a GitPulse session id too, and closing it would close the
shell, so it is refused, as is a terminal that hosts two agents. It is a click in Health, then a
native system dialog that page script cannot answer, with the session checked
again before the dialog and after it. It is **not reachable over MCP** and
nothing is parked on its own. An observed session is never parked, and Codex
cannot be resumed from GitPulse, so neither is offered.

### Owner action: the shim cutover

GitPulse does not edit `~/.claude-bin/cargo`, `~/.zshenv` or any other file
outside its own config. Until the owner puts `<config>/GitPulse/lease-bin`
ahead of `~/.claude-bin` on `PATH` (or has the shim delegate to
`gitpulse-hook lease -- cargo`), builds from a shell GitPulse did not start are
gated by the shim's own slots and not by this. `bun run mcp:doctor` has a
`lease:` row. It asks a login shell which `cargo` it runs with the lease folder
put first on `PATH`, the way an agent's `PATH` is set, so a profile that
prepends another folder shows: `gated` (that is the wrapper), `shadowed by
<path>` (another `cargo` comes first; this never reads as ok) or `absent` (the
links do not exist yet). A login shell that could not be run is `unverifiable`,
not ok.

On this machine `~/.zshenv` re-prepends `~/.claude-bin` for every zsh, so the
row honestly reports `shadowed` until the owner moves that line behind the
lease folder (or `~/.zprofile` reorders it), or retires the shim. GitPulse does
not edit either file. Until then a session is gated through the env-file line
alone, and a build from a shell that line does not reach is not.

### Not verified

- `$CLAUDE_ENV_FILE`: the documented behaviour (SessionStart hooks may append
  `export` lines that later Bash commands see) is what the code relies on. It
  has not been exercised against a live Claude session here.
- The doctor's login-shell probe assumes the shell's `PATH` reflects what an
  agent's Bash tool sees; Claude's own shell snapshot ordering was not checked.
- Linux and Windows: the lease code is `cfg(unix)`; nothing was run there.
- `quiet_ms` comes from the in-process terminal host. A `gitpulse-ptyd` host
  that predates it reports none, so those sessions are "not known".
- The idle thresholds (900 s, 3600 s, 5% of a core) are a starting guess, not
  measured against real sessions.
- No automatic reclaim of build caches and no automatic park exist.

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

## Disk guards

Free space is read two ways, because the 2026-11-15 coverage build took the
disk from about 150 GiB to nothing in about fifteen minutes (about 10 GiB a
minute), and an earlier one took it from 99 to 45 GiB in twenty (about 2.7 GiB a
minute); the level rule alone says nothing until the floor is near.

**Floors follow memory.** The `low_disk` thresholds are automatic: severe is the
larger of 10 GiB and a quarter of physical memory, elevated the larger of 20
GiB and half of it (32 and 16 GiB on a 64 GiB Mac). Whether they are automatic
is a stored fact (`Thresholds::disk_auto`), never inferred from the numbers: a
pair a person sets is kept as set, even when it equals the old 20 and 10, and
wins over any memory size. Settings shows the numbers the governor is deciding
on. A `disk` entry written before this existed has no `auto` marker and stays
the person's own.

**The fill rule** (`disk_filling`, `sysload/diskrate.rs`, `thresholds.rs`). The
writer keeps a bounded history of free space and publishes two rates, in bytes
of free space lost per minute, as readings in the snapshot. The pure assessment
turns each into minutes until the severe floor and compares it with a horizon
(30 and 10 minutes by default, each editable, with a mode of its own: off,
slow, or also refuse). The two tiers ask different questions, because the
original defect was a burst *refusing* launches while shedding on a burst was
always acceptable:

* **Elevated** uses the *quick* rate: the smaller of the least-squares falls
  over the last minute and the last three, or the leave-one-burst-out rate
  when that is larger.
* **Severe** uses the *sustained* rate: the larger of the leave-one-burst-out
  rate and the net fall over the last ten minutes. Leave-one-burst-out (three
  minutes) is the net fall less the largest net fall inside any 60 s
  sub-window, over the window less those 60 s: zero for one burst of a minute
  or less however large, exact for a steady fall, and positive across pauses.
  The net fall divides by the longer of its history and five minutes, so a
  cold start does not inflate it and a lone burst reads as at most its size
  over five minutes. Severe needs the floor under ten minutes away, so a lone
  burst binds it only when the fall exceeds half of the headroom above the
  floor that it leaves, on a cold history (under five minutes), and all of it
  on a warm one (ten minutes). Pinned by
  `a_lone_burst_binds_severe_at_half_the_headroom_cold_and_all_of_it_warm`:
  on a 64 GiB Mac a 26 GiB burst (0.45 of the headroom it leaves) does not bind
  on a cold history and a 30 GiB one (0.56) does; warm, 80 GiB (0.77) does not
  and 100 GiB (1.19) does. A figure of a third on a cold history was not
  reproduced: the test measures half.

**Leaving a fill-caused Severe.** A Severe the fill rule caused is left only
after the fill rule has read below Severe for 180 s without a break
(`FILL_SEVERE_RELEASE`, 90 ticks), not the five ticks every other exit takes;
the level, memory and CPU exits are unchanged. The hold follows the fill rule's
own reading, whichever cause the held binding names: the governor counts the
ticks since the fill rule last read Severe (armed once it read Severe for the
three ticks entering a tier takes, so a one-tick blip under another cause's
Severe arms nothing), and Severe is left only when both the ordinary five
ticks and that count have run out. When memory pressure, swap or the disk level
is Severe at the same time as the fill, the hold still applies; while it alone
keeps Severe (every current reading lower) the verdict names the fill rule and
says it is held after the reading recovered, not that a threshold is crossed.
The count is forgotten when the sensor goes unavailable and does not advance on
a re-read snapshot. The hold is disarmed when its 180 s run out, so a Severe
another cause keeps is never labelled as the disk filling after that, and one
fill-Severe blip hours later arms nothing: arming again takes three in a row.

**Is the disk the only reason?** `sysload::shed_is_disk_only()` answers it for
a caller that may do disk-freeing work while the disk is the whole problem and
must keep shedding when anything else is pressed too (`shed_cause()` is one
binding picked by priority, so it says disk for a disk Severe with CPU Severe
or thermal Critical beside it). True only when the host is shedding, every
other input (memory level normal, no compressor thrash, no other contribution
above Clear, none unreadable) has read Clear for five counted ticks in a row
and reads Clear now, and the disk is what is shedding: a disk input (low disk,
disk filling) reads Elevated or above now, or the fill hold is armed and has
not lapsed, or the held binding is still low disk or disk filling. The last
two matter in a pause of a lumpy fill, where the disk inputs read lower but
Severe is held: nothing but the disk is wrong then, so the cleaner's checks
are admitted. Once the hold lapses and Severe is left nothing is shed and the
answer is false. The count is reset when the run is forgotten and does not advance on a re-read
snapshot. Any doubt is false: a process with no governor of its own (the
headless cleaner run, the MCP server, a hook), a governor built from one
sample (`Governor::instant`), an unavailable or stale-old sensor. The
published snapshot does not carry the count, so a reader of it cannot answer
this.

Running on battery does not count against it: the battery input exists to save
power, not to protect the machine, and spending some battery to free a nearly
full disk is the right trade (an unreadable power source is no more a doubt
than a reading of "on battery"). The tier, the binding and what is shed are
untouched, and a laptop on battery with a healthy disk still sheds background
work and is not disk only. Memory pressure, compressor thrash, compressor
segments, swap, thermal and CPU load keep blocking exactly as before.

**The disk remedy.** `sysload::disk_remedy_due()` is true when
`shed_is_disk_only()` is, or when the low-disk level input itself reads Severe
(free space under the Severe floor, under the configured or automatic floors
and the input's mode) in a fresh reading, whatever else is pressed: a nearly
full disk brings memory pressure with it on macOS because swap cannot grow, so
the remedy must not wait on a symptom of what it fixes. It is for the cleaner's
own work only (its read-only checks, its bring-forward, its scheduled run); it
admits nothing else, and agent launches are refused at Severe as before. It
does not cover the fill-rate projection alone (a burst can project Severe with
plenty of space left) or low disk at Elevated, and it is false on any doubt: no
governor in this process (the headless run, the MCP server, a hook), a sensor
that is off, unavailable or stale, free space unreadable, the low-disk input
turned off. It takes one short lock and does no IO.

**What the hold costs.** A build that writes in lumps with pauses of up to 240 s is held at Severe from
the first bind until free space reaches the floor, where the level rule takes
over, rather than admitting launches in each pause. The cost is a linger: a
finished write that left little room can keep refusing for at most the
ten-minute net-fall window plus the 180 s hold, about 13 minutes (tested bound
800 s). Measured on the real tracker and governor: 12 GiB over 30 s ending at
28 GiB free refused for 742 s warm and 748 s cold; 20 GiB over 80 s ending at
40 GiB, 276 s warm and 514 s cold; 40 GiB over 120 s ending at 67 GiB, 288 s
warm and 454 s cold; a burst of a minute or less on a warm
history that leaves 42 GiB or more free never refuses. Deleting the output
also waits out the 180 s. Writes slower than 3 to 4 GiB a minute on average
(20 GiB per 300 s, 30 GiB per 600 s from 150 GiB) are reported by the tests,
not tuned for.

**Which clock.** The history resets on a gap of more than ten seconds between
samples, so a machine that sleeps comes back with an empty history and the
rates read *warming up* until it has refilled. The gap is measured on the
sampler's monotonic clock (`CLOCK_MONOTONIC` on macOS, `CLOCK_BOOTTIME` on
Linux, per the code comments); whether macOS's monotonic clock counts sleep was
not verified, and either way the first tick after a wake has a gap over ten
seconds from the sampler's own cadence.

What was rejected, from the model behind this: fitting the whole window reads a
step of h GiB as about 1.5 h over the window and refused launches for a burst
that was already over; requiring every slice of the window to fall reads zero
whenever a pause covers a slice, so 5 GiB every 90 s never bound. A build that
writes in lumps with pauses (5 GiB every 60 to 90 s, 8 GiB/min for 40 s of every
120) is sustained by the net fall whatever the pauses: the tests ride through
pauses of 80 s, and the net fall keeps a lump for ten minutes. A write of a few minutes
(20 GiB/min for two minutes) is a fall while it lasts, and refuses then.

Unknown is never a rate. Too few samples, a gap in the timestamps (a sampler
that stalls over ten seconds every two minutes never builds a history), a new
boot and a step no disk can make all leave it `NotYet`. Each reason is its own
state, decided in Rust and worded by Health and the Settings row: *warming
up*; *off* (`host_load.enabled` is off); *older build* (a record written before
the readings existed); *not available on this platform*; *can't be read* (from
the first tick). The rule is silent in each, and a measured zero says "not
falling". The rates and the exact binding are in the trace record.

**What an older reader does.** The snapshot's verdict byte carries `low_disk`
for a disk-filling binding, which every build reads; the exact binding rides in
spare bytes of the extension block. A byte an older build does not know would
make it treat the verdict as unavailable (fail open: "did NOT load-check"), so
the byte it does know is what it reads.

**The command gate's disk checks** (`hooks/load_gate.rs`) read free space live,
on the volume the command writes to: the `cwd` the hook is given, which for an
orchestrated child terminal is not the hook process's own directory, and any
build directory the line names literally and absolutely (`--target-dir
/abs`, `--target-dir=/abs`, `CARGO_TARGET_DIR=/abs` ahead of the program),
read at its nearest existing ancestor; the smaller reading decides. A `cwd`
that was given but is relative, missing or unreadable is *could not read*
(said once per session, never a refusal and never "has room"): the process's
own directory is used only when no `cwd` was given. They refuse a coverage
build below its headroom (60 GiB by default) and any heavy build below the
severe floor. A coverage build is a `cargo llvm-cov` that builds anywhere in
the line (also by its own binary name, `cargo-llvm-cov llvm-cov`) or a cargo
build or test run with `RUSTFLAGS` containing `-C instrument-coverage` (also `-C instrument_coverage`, `-Cinstrument-coverage`) ahead
of it; `clean`, `report`, `show-env`, `--help` and `--version` build nothing
and are never refused. They follow the same two switches as everything else
here: with `host_load.enabled` off nothing is measured and they say nothing,
and with `host_load.govern_launches` off they advise and never refuse.

**Not covered, by name.** `cd /elsewhere && cargo build` writes to
`/elsewhere`, which the hook cannot know without evaluating the shell; only
the `cwd` it was given and the literal build directories above are read.
`sudo`, `xargs`, `env --chdir`, cargo aliases, a build directory the line names
through a variable or `~` (`--target-dir ~/x`, `--target-dir=~/x`,
`CARGO_TARGET_DIR=$HOME/x`), which is dropped and the line is then judged on
the `cwd` alone,
`export CARGO_TARGET_DIR=/x; cargo build`, a `CARGO_TARGET_DIR` already set in
the session, `build.target-dir` in `.cargo/config.toml`, `RUSTFLAGS` exported
earlier in the line, `CARGO_ENCODED_RUSTFLAGS`, and `eval "$(cargo llvm-cov show-env ...)"; cargo test` are not
seen: that last one runs an *instrumented* build after a `show-env` that
itself builds nothing, and it gets only the ordinary floor check, not the
coverage headroom. Wrappers seen through: `env`, `time`, `nice`, `nohup`,
`exec`, `command`, `timeout <duration>`, `caffeinate`, `rustup run
<toolchain>`, `npx`, `pnpm`/`npm`/`bun` exec forms, and `bash -c` with option
clusters such as `-o pipefail` or `-euo pipefail`.

**Storage.** The guards (`fill`, `coverage`) are a top-level
`host_load_disk_guard` key of their own, not inside `host_load_thresholds`: a
build from before them rewrites that key from the inputs it knows and would drop
them. That older build also drops the `auto` marker when it saves
`host_load_thresholds`, which pins the floors at 20 and 10 GiB until the row is
set back to automatic.

## Disk: two owner decisions

2026-10-21: low disk is advice, never a refusal. It can skip the optional
build-cache copy of a worktree (`LOW_DISK_FLOOR`, `Advisory::LowDisk`), which is
unchanged.

2026-11-14: the governor's guards become configurable with protective defaults.
Free disk is one of them (`low_disk`): with mode `refuse` (the default) and free
space below the severe threshold (10 GiB then; now the larger of that and a
quarter of physical memory) a launch is refused with that binding;
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
