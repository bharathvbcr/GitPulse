# Terminal host (`gitpulse-ptyd`)

Card: *Terminal sessions survive a GitPulse app exit or crash: move PTY
ownership into a separate PTY host process* (`ft-1c36bec1d71ba700f65fbfbb12015e71`,
lane L6 of the 2026-10-12 terminal crash-resilience audit, wave 11 of the
audit-2026-10-13 plan). Written against `main` at `8026dd7e`.

## The problem

Every PTY master, child handle and reader thread lives in the GitPulse app
process (`TerminalSessions`, `src-tauri/src/terminal/mod.rs`). When that
process dies — OOM, SIGKILL, Cmd-Q, a panic in AppKit or tao — the masters
close, the kernel hangs up every session leader, and every hosted agent dies
with it. The 2026-10-12 hardening contained faults *inside* the process
(reader panics, poisoned locks, observer panics) and the external mass kill
(signal guard, incident record, Resume). It cannot contain the app process
itself dying. Only moving the masters out of the app can.

## The shape

```
 webview ──Channel──▶ app (TerminalSessions facade) ──unix socket──▶ gitpulse-ptyd
   ▲  cmd_terminal_*        │  proxies, lanes, observers,                │ engine: openpty, spawn,
   └──ack───────────────────┘  incidents, emits                          │ reader, OutputFlow, ring,
                                                                         │ kill/reap, exit receipts
```

* **The engine** is the code that exists today: `spawn_session_inner`, the
  reader loop, `OutputFlow`, `kill_session`, `terminate_pty_child`,
  `session_context`. It moves *as a unit* into the host process. It no longer
  takes a Tauri `AppHandle`: everything it used one for (`terminal-loss`,
  `terminal-exit`, incidents, alert banners) goes through a `SessionEvents`
  value the caller supplies, so the same engine runs in the app (in-process
  backend) and in `gitpulse-ptyd` (host backend).
* **The host** is a new binary, `gitpulse-ptyd` (Rust, `src/bin/gitpulse-ptyd.rs`).
  It is not a mode of `gitpulsed`: that daemon's contract is that it *does not
  serve requests* and holds no children past a cycle, and its single-instance
  lock is per profile, not per protocol. A request server that owns
  long-lived children is a different process with a different lifecycle.
* **The facade** is `TerminalSessions`. Every caller keeps holding it and
  calling the same `terminal::` functions; inside, it is either the engine
  (in-process) or a client of the host. `commands/terminals.rs`,
  `workbench/terminal_run.rs`, `teardown.rs`, `desktop/mod.rs` and the
  frontend contract are unchanged in shape.

## Protocol

A unix-domain `SOCK_STREAM` socket, one frame at a time in each direction.

* A frame is a 4-byte big-endian header length, that many bytes of JSON (the
  typed header, at most 512 KiB — a spawn request carries argv and environment,
  each bounded at 128 KiB by `validate_pty_options`), a 4-byte big-endian
  payload length, and that many raw bytes (at most 64 KiB). Output and
  keystrokes travel as the payload; nothing is base64-encoded (the cost
  `3b1cf774` removed from the webview hop is not reintroduced on this one).
  A frame out of these bounds, or a header that does not decode, ends the
  connection. (`terminal/host/proto.rs`)
* App → host requests carry an `id` and get exactly one reply
  `{"kind":"reply","id":…,"ok":…}` or `{"kind":"reply","id":…,"error":"…"}`.
  `ack` is answered too, inline in the host's read loop, so an acknowledgement
  the window refuses (a late one, for a window a detach already forgave) is
  refused to the view exactly as in-process. The cost is the "second
  flow-control layer" the card warned of: an acknowledgement is a round trip
  (`cmd_terminal_ack` runs it on the control pool, not on an async-runtime
  worker). Requests: `hello`, `retire`,
  `spawn`, `write`, `resize`, `ack`, `detach`, `detach_all`, `attach`,
  `attach_tracked`, `kill`, `stop_all`, `list`, `context`, `background`,
  `limit`, `stall`, `receipts`, `forget`, `transcript` (protocol 3, see
  [Saved transcripts](#saved-transcripts)), and the test-only `wakeups`, `size`,
  `hold`, `release`.
* The app keeps each session's input and geometry lanes in front of the host,
  as in-process: a session's keystrokes go one at a time in order, a full
  queue is refused at once, and a call dropped before its turn is skipped
  rather than sent. The host queues what arrives on its own lanes for the
  session, so a close waiting out its hangup grace never holds up typing.
* Host → app events: `output` (raw bytes for a session's current view),
  `end` (the session's last byte has been sent), `loss` (output a live view
  will never see), `notice` (an alert the reader's scanner raised, its
  `event` and `channel` read back against their closed vocabularies),
  `scan_drops`, and `exit` (the authoritative exit payload).
* `hello` and `retire`, and the reply to each, are the only frames whose
  layout is frozen across versions: `{"id":…,"op":"hello","protocol":N,"build":"…"}`
  answered with `{"protocol":N,"build":"…","pid":…,"sessions":…}`, and
  `{"id":…,"op":"retire"}` answered with `{"retired":…,"sessions":…}`.
  Everything else may change with `PROTOCOL` (`hello_and_retire_keep_their_frozen_shapes`).
* The app's calls are bounded: a deadline per call (10 s; 30 s for a spawn,
  15 s for a close, 30 s to stop everything), failed by a sweeper when it
  passes, and every call in flight fails at once when the connection drops.
  The host's writes to the app are bounded too: a write blocked for 5 s
  (`WRITE_DEADLINE`) means a stopped or wedged app, and its connection is
  closed — which detaches every view, as the app's death would.
* One app at a time. A second `hello` while one is connected is refused with
  a reason; the next is welcome once the first's connection ends.

## Flow control across two links

Today `OutputFlow::reserve` blocks the reader until the webview repays the
512 KiB window, and a detached session's bytes are dropped
(`Route::Hold`, mod.rs:1785). With a host in between:

* **The window stays where the reader is, in the host, and spans both links.**
  The app relays output frames to the view's `Channel` without buffering and
  relays the view's acks back as `ack` frames. A byte is outstanding from the
  moment the host reserves it until the webview acknowledges it; the socket can
  therefore never hold more than one window per session, and there is no
  second window to keep in step.
* **The host never blocks on the app being alive.** When the app's connection
  ends, the host detaches every session — exactly what a page reload already
  does (`detach_all`) — and a detached session's reader holds instead of
  waiting. No app-side state is needed for the host to keep draining.
* **The ring replaces "dropped while detached".** Every byte the reader reads is
  appended to a per-session ring *before* it is routed, whatever the route.
  `attach` replays the ring's tail through the normal delivery path before live
  output resumes, then nudges the size as before so a full-screen program also
  repaints.
  * The ring is **512 KiB** — one output window — not the card's suggested
    1–4 MiB. The view acknowledges a replay only after `attach` has returned
    (the frontend binds a channel to its session id once the call resolves), so
    a replay that needed more credit than one fresh window would wait out the
    30 s stall and then be shed. A ring larger than what can be replayed is
    memory nothing reads.
  * When more was printed while detached than the ring kept, the view is told
    through `terminal-loss` ("output while detached was not kept") before the
    tail is replayed. This is the S2-11 marker, and it is shown only when bytes
    were actually lost.

## Lifecycle

* **Where:** `<config dir>/pty-host/` (`~/Library/Application Support/GitPulse/pty-host`
  on macOS), created `0700` and refused if any component is a symlink. The socket
  is `ptyd-v<PROTOCOL>.sock`, `0600`, bound through `local_socket::bind_private`
  (the one hardened bind the agent bridge and the instance mailbox already
  share). A path too long for `sun_path` is reported by name, as it is there.
* **Single instance per protocol:** the host holds the `daemon_lock` lock on
  `ptyd-v<PROTOCOL>.lock` in that directory for its whole life. A second host
  started for the same protocol exits at once and says who holds it.
* **Start:** the app connects at startup (before it reconciles task runs) and
  on its first terminal call; when nothing answers, it starts `gitpulse-ptyd`
  from beside its own executable (`insights::bundled_binary`, the owner that
  already finds `gitpulse-mcp` and `gitpulse-hook` in `Contents/MacOS`, where
  the bundler places every `[[bin]]`; `GITPULSE_PTYD` overrides it) with
  `setsid`, stdin and stdout on `/dev/null` and stderr on `ptyd.log` in the
  host directory. It is not spawned through `procguard::spawn`: the quit sweep
  (`procguard::reap_all`) kills what that tracks, and the host must outlive the
  app. The app reaps it if it exits first, and never signals it.
* **Trust:** the host reads the same trust store the app writes (it inherits
  `HOME`). A grant the user saved reaches it; a *session-only* grant, which
  lives in the app's memory and by design never grants another process
  authority (`repository_trust::grant`), does not. The app's own approval flow
  always saves (`grant_confirmed`), so this only matters to tests, which save
  their grants when they run against the host.
* **Idle exit:** a host with no sessions and no app connected for 30 s exits
  and removes its socket, so a host is never left behind for nothing.
* **Host SIGTERM / SIGINT / SIGHUP:** the host stops every session with the
  same hangup-grace close the app uses today, records each exit, and exits.
* **Not done here:** launchd registration (a `KeepAlive` agent), which the card
  asks for. A launchd restart of a crashed host would not bring its sessions
  back, so it buys nothing for this card's failure; `setsid` is what keeps the
  app's death from reaching the host, and the SIGKILL test proves that. It
  remains open, not claimed.

## Version skew

The socket name carries the protocol version, so an app never speaks to a
host of another protocol by accident.

* At connect, the app sends `hello`. A host that answers with another protocol
  than the app's (only possible if the file was renamed by hand) is refused,
  and every terminal call says so.
* The app also looks for sockets of *other* protocol versions in the host
  directory and sends each the frozen `hello`. A live host of another protocol
  is reported loudly — logged as a warning, and shown under Settings → Agents →
  "Keep terminals running when GitPulse quits" (`cmd_terminal_backend`'s
  `trouble`) — with its build, pid and session count:
  * with **no sessions**, the app sends it `retire` (also frozen) and it exits;
  * with **sessions**, it is left running, untouched, and the report says how
    many sessions it holds and that this version cannot show them. Nothing is
    killed and nothing is silently ignored.
* The same protocol with a different build is compatible by definition and is
  logged with both builds.

## Durable exit receipts

The host writes one append-only file per session,
`<host dir>/receipts/<session id>.jsonl` (`0600`, opened `O_NOFOLLOW`, named
only by a well-formed session id): a `spawn` line just before the child starts
(id, run id, program, place) and an `exit` line, synced to disk, when it is
reaped. The exit is then sent to the app as an `exit` event.

* The app processes an exit (run observer, incident, `terminal-exit`) and then
  sends `forget`, which deletes the file. A crash between the two replays the
  receipt at the next connect; the workbench write is idempotent by its fixed
  request ids (`receipts.rs`).
* At connect the app asks for `receipts` — every session that ended while no app
  was there — and processes each as if it had just exited. For a task run, the
  receipt goes through the same `receipts::apply` the live observer uses, with
  the owner and start identity read back from the run itself.
* A `spawn` line with no `exit` line and no live session (the host itself died)
  is given an exit when the next host starts, with `reaped: false` and the
  error "The terminal host ended before this session did; its exit status is
  unknown.", which `receipts::apply` records as `unresolved`, never as a clean
  exit. A session the app still tracked that neither the host's list nor a
  receipt accounts for is ended the same way at connect.
* A run whose own observer died with the app that launched it is observed again
  by `terminal_run::adopted_observer`, which reads the owner, session and
  process identity back from the run. It is used for a session the next app
  adopts and for a receipt it records.
* An exit can overtake the reply to its own spawn (a program that ends at
  once). The app holds such an exit until it has recorded the start, so a run
  is never finished before it is started.
* **Exit recording and adoption ordering:** `record_exit` is invoked strictly once
  per session. `adopt` skips reported exits and reads `vouched.late` after its proxies
  scan, ensuring late-arriving exits do not race with adoption. A second exit report
  arriving during recording leaves both proxy and host state intact, so adopt never
  ends a session that was confirmed during adoption.
* **`Host::knows` client-side vouched snapshots:** To eliminate IPC round-trips
  to `gitpulse-ptyd` for session membership checks, `Host::knows` answers directly
  from the host client's vouched snapshot. Authority survives spawns, quits, and
  wrapped exit records mid-adoption, while newly opened connections cleanly revoke
  previous authority. A delivered exit is not vouched for immediately until verified.

## Saved transcripts

The ring keeps 512 KiB in memory and the receipts keep how a session ended;
neither keeps what it printed. A host that dies, a reboot, or simply a run that
finished while nobody watched lost the output. The host now also writes it down:
`<host dir>/transcripts/<session id>.log` (`terminal/host/transcripts.rs`), so a
finished task run's output can be reopened after the app quits or the host
restarts (`cmd_terminal_transcript`, `src/lib/terminal/savedTranscript.ts`).

**What is recorded.** Every byte the reader reads, held or not. The copy is taken
in `Ring::push` (`terminal/ring.rs`), the one place that sees all of a session's
output: a detached session's bytes never reach the output sink, so a tap on the
sink would miss exactly the output this exists for. The tap is armed before the
fork (`Taps`, claimed when the session's ring is made), so a program that prints
and exits at once is recorded from its first byte. Only the host records; the
in-process backend keeps nothing, and a read says so (`unavailable`), not
`missing`.

**The reader never waits on the disk.** `Ring::push` runs with the ring's lock
held, and that lock is held across the wait for the view's credit, so the tap
only appends to an in-memory queue bounded to 4 MiB (each queued operation
counts for its bytes plus 64). One supervised writer thread (`spawn_service`,
named `terminal-transcripts`) drains it. When the queue is full the chunk is
dropped, its size remembered, and `[gitpulse: N bytes of output were not
recorded]` is written ahead of the next chunk that fits (or when the session
ends). Recorded bytes plus the counts in the markers always add up to what the
session printed.

**Bounds** (`Limits`):

* a session's log is two segments, `<id>.log` (current) and `<id>.log.1`
  (previous), at most 4 MiB each; a full current segment becomes the previous
  one and the old previous one is deleted. Every segment after the first begins
  with `[gitpulse: transcript continues]`, which a read strips from the text and
  uses to know the start of the output is gone (`truncated`);
* the directory is held to 256 MiB by deleting whole sessions, least recently
  written first, never one still running (checked when a segment rotates and
  when a session ends). If running sessions alone are over it, their previous
  segments go next, and a warning is logged while they remain over;
* a transcript untouched for 14 days is deleted when a host starts and each time
  a session ends or a segment rotates (the same pass that enforces the size
  limit), so a host that stays up ages them out as long as sessions keep ending;
  an idle host that never ends another session keeps them until it exits;
* writes are plain `write_all`s to the page cache and survive the host process
  dying. They are `fsync`ed when a segment rotates, when a session ends, when
  the host stops, and 5 s after the last write to an idle session, so a power
  loss or kernel panic can lose only the unsynced tail of a running session.

**Lifecycle, and what "tied to the receipt" means.** The file is created when
the session is armed, beside its receipt's `spawn` line; it is closed and
synced when the session's ring is dropped; and it is removed with the receipt
when the session never started (`discard`). It is *not* removed by `forget`:
that means "the app has recorded this exit", which is long before anyone reopens
the run's output, so deleting there would leave the feature with nothing to
show. A finished session's transcript ends by eviction or age, above. A new
session under an id that has a transcript replaces it.

**How the recording ended is on disk.** Whether a transcript is whole is not
something a host's memory may hold: the host that knew is the one that died.
Closing a session appends `[gitpulse: transcript ends]` to its newest segment
(under the segment's size bound; a segment with no room rotates first), and a
write that fails leaves `<id>.log.err` with the reason (`0600`, `O_NOFOLLOW`,
at most 2 KiB, removed and evicted with the session). A read strips the marker
like the continuation line and answers `recording_error`: the failure's reason
if there is one (from memory, or from the note after a restart), else, for a
finished session whose newest segment lacks the marker, *the host that wrote
this did not close it, so the end of the output may be missing*. A session that
is still running has no marker yet and is not reported for it. The marker is
text a program could print; spoofing it can only make a transcript read as
closed.

**The read.** `Request::Transcript { session, max_bytes, redact }` is read-only
(`attach` means a live session; this never attaches) and works for live and
finished sessions. `max_bytes` is clamped to 128 KiB. The tail crosses the
socket as base64 inside the reply header (the same `data_b64` the webview hop
has always used for a one-shot `{id, data_b64}`), not as a frame payload: the
streaming `output` path is unchanged and stays base64-free, and a reply
carrying a payload would need a payload slot threaded through every waiter for
a call that happens once per reopened run. 128 KiB is 171 KiB encoded, under
the 512 KiB header bound. The answer (`terminal::TranscriptRead`) is one of:

| state | meaning |
|---|---|
| `found` | the tail, plus `bytes` (after masking, when `redact`), `retained` (on disk, before masking), `truncated` (`rotated` or `clipped`: `rotated` means the start was rotated off the disk, `clipped` that older saved output was left out of this read's bound; both can hold), `gap_markers`, `redacted`, `live`, `settled` (everything queued was written before the answer; false means the end may lag) and `recording_error` (the saved output may stop short: a write failed, or the host that wrote it never closed it) |
| `missing` | no transcript: never recorded, evicted, or aged out |
| `unreadable` | one exists or should and could not be read; `reason` says why. A transcript that is a symbolic link, is not a regular file, belongs to another user, or could not be recorded (disk full, directory gone) is `unreadable`, as is a host whose transcript store would not open |
| `unavailable` | the app runs terminals itself (never sent by a host) |

The renderer's `readSavedTranscript` adds `failed` (the call failed, timed out,
or the answer was malformed), which is not `missing`: the output may be there.
`recording_error` survives a host restart (see *How the recording ended is on
disk*); a transcript whose host died reads as `found` up to where it stopped
with `recording_error` set, never as complete.

**Protocol.** Adding the request bumped `PROTOCOL` to 3. A host of protocol 2
ends the connection of an app that sends a request it cannot decode, so the two
must never meet; they do not, since the socket and lock names carry the
protocol. An updated app finds a v2 host the way it finds any other-protocol
host (`probe_other_protocols`): retired when idle, reported with its session
count when it holds sessions.

**At-rest posture.** The transcript is the raw terminal stream, escape sequences
and anything a program echoed included, so it can contain secrets. It is stored
unredacted, deliberately: redaction would corrupt the byte stream (a secret cut
across a chunk is missed in one place and a mask lands inside an escape
sequence in another), and the file is reachable only by the user who owns it.

* The directory is `0700` and refused if any component is a symlink; each file is
  created `0600`, opened `O_NOFOLLOW`, checked to be a regular file owned by the
  host's user (a file with wider permissions is narrowed before anything is
  written), and named only by a well-formed session id. A read applies the same
  checks, so a link planted in place of a transcript is reported, never followed.
* The only way to it through GitPulse is the host socket, which refuses any peer
  that is not the host's own user. It is never sent anywhere.
* `redact: true` runs the tail through `ledger::redact::text` in the host, before
  the bytes leave it, and is what any surface beyond the local user (an export, a
  ledger entry, a shared report) must use. A masking read looks 64 KiB further
  back than the window it returns, masks, then clips to the window, so a name
  such as `password=` or a key's first line just before the window still marks
  what follows. A private key whose closing line is in that margin but whose
  opening line is not is masked up to the closing line. What it cannot see: a
  secret split by the margin's own start, or a key body longer than the margin
  with neither line in view. Redaction is a mask for display, not a guarantee.
* `keep_terminals_running` stays default-off; a user who never turns it on has
  no host and no transcripts. The sign-off in the security statement below covers
  what a transcript at rest widens.

## Quit

With the host backend a quit no longer has to end anything, so it asks:

* **Cmd-Q with live sessions** asks (`quitChoice.ts`): *Quit and Leave
  Running* (the default button), *Stop Them and Quit*, or *Keep Working*. A
  session list that cannot be read still asks, saying so. The answer reaches
  the backend as `cmd_exit_app`'s `terminals` (`"leave"` or `"stop"`).
  *Leave running* disconnects; the next launch adopts them as detached
  sessions (`adoptDetachedSessions`) and showing one replays its ring.
  *Stop* stops every session, records each exit, then disconnects.
* **SIGTERM / SIGKILL / a crash of the app** leaves sessions running (that is
  the point of the card): the teardown for a signal, like a quit that did not
  choose, disconnects instead of stopping (`teardown::close_terminals`).
* With the in-process backend nothing changes: quitting stops every session.

## Managed runs in a hosted Manvi

Managed Codex and Claude runs are not PTY sessions: Manvi drives them, and they
live in the memory of the `manvi serve` that GitPulse spawns, so closing that
child cancels every run. The same setting (`keep_terminals_running`) therefore
also hosts that child outside the app, in Manvi's socket mode
(`manvi serve --listen`, `docs/SERVE_HOST_PLANE.md` in Manvi). Code:
`harness/hosted.rs` (paths, record, capability, the account Settings reads) and
`harness/sidecar.rs` (the connection, which speaks to the socket exactly as to a
pipe).

* **When.** Only for the app's own profile worker, and only with the setting on.
  With it off, or on an installed Manvi that predates `--listen`, the worker is
  the stdio child it has always been, and Settings → Agents says why on a line of
  its own (`cmd_terminal_backend`'s `managed`): *"Managed agent runs end when
  GitPulse quits: the installed Manvi (…) cannot serve on a socket; update
  Manvi."* Support is read from `manvi serve --help` naming `--listen` (starting
  nothing, once per binary file), and checked again from the running server's
  `hello` (`serve.status` and `serve.stop_all` must be served). The policy and
  hook sidecars never host runs and are untouched.
* **Where.** `<config>/GitPulse/manvi-host/`: the socket (`serve.sock`), the
  server's persistent state directory, `serve.log`, and `host.json`, the record
  GitPulse writes when it starts a server (socket, owner id, pid, the model
  selection it was started with, the profile). The server is started detached
  (`setsid`) and outside `procguard`, like `gitpulse-ptyd`, so no quit sweep
  reaches it; it exits by itself after five minutes with no client and no
  running run.
* **Quit.** With a hosted Manvi holding working runs the question is the
  terminals' (`decideQuitManagedRuns`): *Quit and Leave Running* (default),
  *Stop Them and Quit*, *Keep Working*, sent as `cmd_exit_app`'s `managed`. Leave
  disconnects and nothing else; stop sends `serve.stop_all`, waits at most ten
  seconds, then disconnects. Without a hosted Manvi the old two-way question
  stands, and its confirmation is sent as `managed: "stop"` too, never left
  absent: absent means *leave* to a backend that does hold the runs, and the
  reading that chose the two-way question can be wrong (a connection that was
  dropped reads as not hosted). When the worker holds no connection at that
  point (a relaunch in which nothing has needed the worker yet, so none was ever
  made; a replaced or faulted connection) a stop dials the server at its own path (adopting only, never
  starting one, with every check of the relaunch below), bounded by the
  controller wait and the stop's own wait, and stops the runs. A server that
  cannot be reached or used leaves its runs going; that is logged, not hidden.
  An exit nobody was asked about (a signal, a crash) leaves the runs, as it
  leaves the terminals. The stop round trip never runs under the worker's lock
  or the workbench's, and `teardown::steps` settles any hosted connection the
  worker's own shutdown did not reach. Whether the app is "hosted" is the last
  observation of the connection that made it: an older connection ending late
  cannot take it back from a newer one.
* **Relaunch.** Reconciling task runs asks the hosted Manvi first, for a managed
  run that still holds its checkout. The server is dialled only at its own
  path, and only if that is a socket owned by the user answering from the user's
  uid; `serve.status` is read before anything else (a connection that is not the
  controller is answered nothing but status), its protocol and owner must be
  well formed and agree with `host.json`, and its profile must be this one. Then
  the worker is connected to it, so the run can be stopped from the board as
  before, and the server's job list decides: a run it lists is *adopted* and
  kept; a run it does not list, when the list may be the whole list and no agent
  process was recorded for it, is released (a hosted Manvi outlives its runs, so
  "the Manvi is alive" no longer says the run is). No socket, a socket nobody
  answers on, or a server of another owner or profile adopts nothing and leaves
  today's release rules in force. A server that could not be read at all keeps
  every verdict as it was: unobserved is not gone. The server is asked once per
  reconciliation pass, not once per held run, so a server controlled by another
  client costs one bounded wait and not one per run.
* **Another controller.** A second connection to a server is answered busy until
  the first's is served out (Manvi allows five seconds), so a reconnect waits a
  bounded time. After that the server is reported as controlled by another client
  and is **not** taken over (`serve.takeover` is never sent): GitPulse is a
  single-instance app and a dead GitPulse's connection is closed by the kernel,
  so another controller is something else, and cancelling its requests is not
  ours to do. Nothing is started beside a server that cannot be used — two
  Manvi processes on one profile would each run the background work — and
  Settings says so.
* **A model or key that changed.** The server keeps the environment it was
  started with. A new model selection or Claude key reaches it only through a
  new server: an idle one is ended (SIGTERM to the pid it reported over the
  authenticated socket, that `host.json` names, whose start time still equals
  the one recorded when GitPulse started it, never this process) and a fresh one
  started; one that holds runs is kept for them,
  `work.enhancements.generate`/`refine` answer `model_pending` meanwhile, and the
  next call after the runs end replaces it. This holds for a server adopted at
  relaunch exactly as for one this connection started: the selection it was
  started with (which also says whether it holds the Claude key) is the record's,
  and it is judged against the selection the connection wants.
* **A server that cannot be used.** A server this app starts and then cannot use
  (it never opens its socket, answers for another protocol, or does not serve
  what is needed) is ended with the same guarded SIGTERM before the stdio child
  runs, so the child is not beside a detached server nothing will connect to. It
  held no run. If its start time could not be read it is not signalled and idles
  out after five minutes (logged).
* **Turning the setting off.** The setting is read when the worker connects. If
  it is turned off while a hosted server still holds runs, the next launch uses
  the stdio child: those runs keep going and liveness keeps their checkouts, but
  they cannot be controlled from the app until they end, and a quit does not
  dial the server. Settings then describes the stdio child's behaviour (runs end
  with the app), which is not true of runs that server already holds; turn the
  setting back on to adopt them.
* **Socket path.** The path must fit both the system's limit and Manvi's own
  (100 bytes); a longer one is refused before anything is started, with the
  reason.
* **Per connection.** Each connection to the server has its own chat ledgers and
  `capability.probe` cache, so a reconnect starts them empty; the managed runner
  and its jobs are the server's.

## Signals

* Every signal the engine sends goes through `procguard::signal`. The jobs
  path already did (`jobs::hang_up`, `jobs::kill_stragglers`); the last
  `portable_pty` `Child::kill` on the session leader is replaced by
  `signal_process` keyed to the start time recorded at spawn.
* `signal_guard` already counts `gitpulse-ptyd` as a GitPulse process (names
  starting `gitpulse`, other than `gitpulse-hook`). It now also refuses a
  signal command that reaches a *session leader* GitPulse hosts — a process
  whose parent is a GitPulse process and which leads its own session — not only
  hosted agents. Jobs running under a hosted shell are not protected: killing a
  dev server you started in a GitPulse terminal stays your business.

## Platforms

Unix only, as the PTY internals already are (`cfg(unix)` around `input`,
`jobs`, `readiness`). On Windows the in-process backend is the only one and the
host binary prints that it is unsupported. A ConPTY host is a separate card.
Gatekeeper: the helper ships beside the app binary in `Contents/MacOS`, where a
signed and notarized bundle carries it; nothing is loaded from outside the
bundle except through the explicit `GITPULSE_PTYD` override that development
builds use.

## Coupling inventory (S2-10)

The audit counted ten points where the app is coupled to owning the PTYs.
Workbench coupling is at `terminal_run.rs:73` and `:159` (the
`&TerminalSessions` parameters of `spawn` and `start`) and
`impl SessionObserver for RunObserver` at `:270` — not `:317-331`, which are
tests. The two clauses that close the audit's list (teardown and page-load
becoming disconnect-or-stop, `quitRisks` trusting the host list) are folded
into the rows they belong to (2, 3 and 4).

| # | Coupling point | Moves to host | Stays in app | Deleted |
|---|---|---|---|---|
| 1 | Workbench: `terminal_run.rs:73` `spawn(&TerminalSessions)`, `:159` `start(&TerminalSessions)`, `:270` `impl SessionObserver for RunObserver` | — | **Stays.** Both functions keep their `&TerminalSessions` parameter (the facade). `RunObserver` stays in the app: it writes to the workbench store, which the host never opens. Its `before_spawn` runs before the spawn request (the app now picks the session id), `started` when the reply names the pid, `finished` when the `exit` event or a receipt arrives. A run whose observer died with an earlier app gets an *adopted* observer that rebuilds `receipts::Observed` from the stored run. | — |
| 2 | `TerminalSessions` `Arc<Mutex<HashMap>>` cloned into teardown (`lib.rs:177-178`, `:625`); `teardown.rs:46` | The session map itself (masters, children, readers) | The facade's clone into teardown and `procguard::set_shutdown_hook`. `teardown.rs:46` (`stop_terminals`) becomes *stop* or *disconnect* by the quit choice; a signal exit disconnects. | — |
| 3 | Page-load detach (`desktop/mod.rs:255-256`) | `detach_all` runs in the host | The call site stays and sends `detach_all`; it is a disconnect of views, never a stop. | — |
| 4 | `cmd_terminal_*` (`commands/terminals.rs`), and `quitRisks` | — | **Stays**, unchanged in signature. Each command calls the same `terminal::` function, which the facade routes. `cmd_terminal_sessions` returns the host's list, so `quitRisks` (`quitGuard.ts`) already treats the host list as authoritative and needs no change. | — |
| 5 | Per-session `Channel` + `OutputFlow` credit, `Route::Hold` dropping bytes (mod.rs:1783-1785), no ring | `OutputFlow` (the window) and the new ring | The `Channel` (a webview object) and the relay that forwards `output`/`end` frames to it and `ack`s back | Dropping detached bytes: replaced by the ring. |
| 6 | `terminal-loss` / `terminal-exit` / incident emits | The decision to emit (reader, finalize) | The emits themselves and the incident `Recorder`, fed by `loss` and `exit` events through `SessionEvents` | The `AppHandle` parameter of the engine |
| 7 | `SessionWatcher::observe` on raw bytes in the reader (mod.rs:759-825, `:1872`) | **Runs in the host's reader**, on every byte as now, so a detached session's bell is still seen while an app is connected | `alerts::offer` runs in the app, fed by `notice` events | — |
| 8 | `session_context`, `foreground`, `getsid`-checked kill | All of it: it needs the child handle and the master's process group, which live in the host. | — | — |
| 9 | Spawn deps: `validate_repo`, `require_unsubstitutable_dir`, `sysload::admit`, `record_session_spawn`, `GITPULSE_SESSION_ID` / `GITPULSE_NOTIFY_SOCKET` env, fixed socket name `agent-notify.sock` | `validate_repo`, `resolve_start_dir`, `task_start`, `require_unsubstitutable_dir` (trust is checked where the fork happens) and the environment (`GITPULSE_SESSION_ID`, `GITPULSE_NOTIFY_SOCKET`, run id) | `sysload::admit` (its "Admit anyway" override is app memory) and `record_session_spawn` (the app's ledger record, after the reply). `agent-notify.sock` keeps its fixed name in the config directory, so a hook in a session that outlived one app reaches the next. | — |
| 10 | Ids `term-<app pid>-<n>` persisted in `hostedClaude` | — | Ids are still minted by the app, as `term-<app pid>-<n>`, and the host refuses a duplicate. They stay unique across app restarts because the app's pid changes, and a session adopted after a restart keeps the id `hostedClaude` stored. | — |

## Security impact statement

**What this widens.** A new unix socket that can start processes, type into
them and read their output. Before, the only way to drive a GitPulse PTY was
the webview's IPC. Now any process that can `connect` to
`ptyd-v<N>.sock` can spawn a shell or an agent CLI in a *trusted* repository,
attach to a live session, read what it prints, and type into it.

**Who can reach it.** Only processes of the same user:

* The directory is `0700` and refused if any component is a symlink; the
  socket is `0600` inside it and bound without touching the umask
  (`local_socket::bind_private`).
* Every connection's peer uid is read (`getpeereid` / `SO_PEERCRED`) and refused
  unless it is the host's own. On a Unix where neither is available the socket
  is not bound at all — a check that cannot run is not reported as one that
  passed.

**What a same-user peer gains.** Nothing it could not already do: a process
running as the user can already start a shell in any directory the user can
read and write. The socket does not grant root, another user's files, or a
repository the user has not trusted. It does let a same-user process type into
a running agent session; such a process can equally edit the agent's settings,
its hooks or the repository directly. A per-connection token would not change
this — it would sit in a file the same user can read.

**Checks that move with the fork.** `validate_repo` (a git work tree, no
`.git` file pointing elsewhere), the trust grant, and
`require_unsubstitutable_dir` run in the host, in the process that forks, on
the canonical path — not in the app before a request crosses a socket, where a
directory could be swapped between check and fork. The app's own checks stay
in front of the request (permission-mode refusals, the launch-flag table, the
load governor), so a request that reaches the host has passed both.

**Bounds.** Header frames ≤ 64 KiB, payloads ≤ 64 KiB, argv/env bounds as
today (`validate_pty_options`), the session cap as today. A peer that sends a
malformed or oversized frame is disconnected.

**Saved transcripts widen what a same-user peer can read.** Before, a peer could
read a session's output only by attaching to it, while it lived. Now it can read
the last 8 MiB per session of finished ones from the socket (`transcript`) or
from the files, for up to 14 days or until the 256 MiB directory bound evicts
them. The same-user reasoning above holds (the files are `0600` in a `0700`
directory), but a transcript outlives the session, and that is new at rest; it
is on disk only when the host is, which is opt-in. See [Saved
transcripts](#saved-transcripts).

**Signals.** The host signals only through `procguard::signal` and only
processes in sessions it created; the app never signals the host.

**Hosted managed runs (Manvi).** With the same opt-in, a `manvi serve --listen`
process outlives the app, holding managed Codex and Claude runs.

* *What it widens.* Agents keep running, with the repository's trust and the
  user's permissions, after the window is closed, and there is a second socket
  that can drive them: `manvi-host/serve.sock` accepts any process of the same
  user that can prepare, activate, stop and read managed runs for the profile.
  Quitting no longer ends an agent the person has forgotten about; it ends only
  when it finishes, is stopped, or the machine restarts. This is the posture the
  setting already has for terminals, and the default stays off for the same
  reason (the sign-off below).
* *What a same-user peer can do with the socket.* Everything the app can: start
  and stop managed runs on the profile's repositories, read what they printed
  (checkpointed in the run record). Nothing it could not do by editing the
  profile database the server uses or by starting `manvi serve` itself. The
  directory is made `0700` by GitPulse (which refuses a symlinked one) and the
  socket is created `0600` by Manvi (GitPulse does not check the socket's mode),
  Manvi refuses a peer of another uid, and GitPulse refuses to dial a socket that
  is not a socket owned by the user, answers from another uid, belongs to another
  owner than the one it recorded, or serves another profile; a platform that
  cannot report a peer's user hosts nothing.
* *Credentials.* A server started for a Claude selection holds the saved Claude
  API key in its environment until it exits (the stdio child holds it for the
  life of the app). It is started only after Manvi's promise to withhold it from
  its children has been read, and is checked again in `hello`. A key that
  changed reaches a server only when it is replaced, which waits for its runs.
* *Signals.* GitPulse signals the server in two cases: a replacement of an idle
  server whose pid it verified three ways (see above), and a server it has just
  started and cannot use, identified by the pid it started and that process's
  start time. Both go through `procguard::signal`, which refuses a pid whose
  start time differs, this process and pid 1. It never signals a server that
  holds a run.
* *Stopping at quit.* *Stop Them and Quit* can now dial the server from an app
  that holds no connection to it. That dial is the relaunch's: only the server
  at GitPulse's own path, owned by the user, answering from the user's uid, of
  the recorded owner and profile, never started, never taken over. It widens
  nothing a same-user process could not already do.

**Sign-off.** This statement needs the owner's review before the host
becomes the default (migration step 3). Until then the host is opt-in.

## Migration

1. **Done in this change:** the engine is decoupled from Tauri; the host binary,
   the protocol, the client and the facade exist; the ring exists in the engine
   for both backends, and exit receipts in the host.
2. **Done in this change:** Settings → Agents, "Keep terminals running when
   GitPulse quits", selects the host for the next launch (default off). The
   integration suites run against the host when
   `GITPULSE_TEST_TERMINAL_BACKEND=host` (`bun run test:terminal-host`); CI runs
   them both ways on its macOS and Linux legs, and `ci:local` does too.
3. **Not in this change:** make the host the default and delete the in-process
   backend (the app constructing the engine itself) in the same commit. Blocked
   on the security sign-off above.
