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

* A frame is a 4-byte big-endian length, that many bytes of JSON (the typed
  header, at most 64 KiB), and — only when the header carries `"len"` — that
  many raw bytes (at most 64 KiB). Output and keystrokes travel raw; nothing
  is base64-encoded (the cost `3b1cf774` removed from the webview hop is not
  reintroduced on this one).
* App → host requests carry an `id` and get exactly one reply
  `{"reply":id,"ok":…}` or `{"reply":id,"error":"…"}`. `ack` is the exception:
  it is sent per acknowledgement, inline, and never answered.
  Requests: `hello`, `spawn`, `write`, `resize`, `ack`, `detach`, `detach_all`,
  `attach`, `attach_tracked`, `kill`, `list`, `context`, `background`,
  `limit`, `stall`, `receipts`, `forget`, `retire`, and the test-only
  `wakeups`, `size`, `hold`, `release`.
* Host → app events: `output` (raw bytes for a session's current view),
  `end` (the session's last byte has been sent), `loss` (output a live view
  will never see), `notice` (an alert the reader's scanner raised), `exit`
  (the authoritative exit payload).
* `hello` is the only frame whose layout is frozen across versions:
  `{"op":"hello","protocol":N,"build":"…"}` answered with
  `{"protocol":N,"build":"…","pid":…,"sessions":…}`. Everything else may
  change with `PROTOCOL`.

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
* **Start:** the app connects; when nothing answers, it starts
  `gitpulse-ptyd` from beside its own executable (`resolve_bundled_binary`, the
  owner that already finds `gitpulse-mcp` and `gitpulse-hook` in
  `Contents/MacOS`) with `setsid`, its standard streams on `/dev/null` and a log
  file in the host directory. It is not spawned through `procguard::spawn`:
  the quit sweep (`procguard::reap_all`) kills what that tracks, and the host
  must outlive the app. The app never signals the host.
* **Idle exit:** a host with no sessions and no app connected for 30 s exits
  and removes its socket, so a host is never left behind for nothing.
* **Host SIGTERM / SIGINT / SIGHUP:** the host stops every session with the
  same hangup-grace close the app uses today, records each exit, and exits.
* **Not done here:** launchd registration (a `KeepAlive` agent). A launchd
  restart of a crashed host would not bring its sessions back, so it buys
  nothing for this card's failure; `setsid` is what keeps the app's death from
  reaching the host. Recorded as a follow-up, not claimed.

## Version skew

The socket name carries the protocol version, so an app never speaks to a
host of another protocol by accident.

* At connect, the app sends `hello`. A host that answers with another protocol
  than the app's (only possible if the file was renamed by hand) is refused,
  and every terminal call says so.
* The app also looks for sockets of *other* protocol versions in the host
  directory and sends each the frozen `hello`. A live host of another protocol
  is reported loudly — logged, listed in Health as a degraded subsystem, and in
  the error of any call that cannot proceed — with its session count:
  * with **no sessions**, the app sends it `retire` (also frozen) and it exits;
  * with **sessions**, it is left running, untouched, and the report says how
    many sessions it holds and that this version cannot show them. Nothing is
    killed and nothing is silently ignored.
* The same protocol with a different build is compatible by definition and is
  logged with both builds.

## Durable exit receipts

The host writes one append-only file per session,
`<host dir>/receipts/<session id>.jsonl` (`0600`): a `spawn` line when the
child starts (id, run id, program, cwd, pid) and an `exit` line, synced to disk,
when it is reaped. The exit is then sent to the app as an `exit` event.

* The app processes an exit (run observer, incident, `terminal-exit`) and then
  sends `forget`, which deletes the file. A crash between the two replays the
  receipt at the next connect; the workbench write is idempotent by its fixed
  request ids (`receipts.rs`).
* At connect the app asks for `receipts` — every session that ended while no app
  was there — and processes each as if it had just exited. For a task run, the
  receipt goes through the same `receipts::apply` the live observer uses, with
  the owner and start identity read back from the run itself.
* A `spawn` line with no `exit` line and no live session (the host itself died)
  is reported as an exit with `reaped: false` and the error "the terminal host
  ended before this session did", which `receipts::apply` records as
  `unresolved`, never as a clean exit.

## Quit

With the host backend a quit no longer has to end anything, so it asks:

* **Cmd-Q with live sessions** shows *Stop sessions and quit*, *Quit and leave
  them running*, and *Cancel*. *Leave running* disconnects; the next launch
  lists them as detached sessions and attaches with the ring replayed.
* **SIGTERM / SIGKILL / a crash of the app** leaves sessions running (that is
  the point of the card). The teardown for a signal disconnects instead of
  stopping.
* With the in-process backend nothing changes: quitting stops every session.

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

**Signals.** The host signals only through `procguard::signal` and only
processes in sessions it created; the app never signals the host.

**Sign-off.** This statement needs the owner's review before the host
becomes the default (migration step 3). Until then the host is opt-in.

## Migration

1. **Done in this change:** the engine is decoupled from Tauri; the host binary,
   the protocol, the client and the facade exist; the ring and exit receipts
   exist in the engine for both backends.
2. **Done in this change:** Settings → Agents, "Keep terminals running when
   GitPulse quits", selects the host for the next launch (default off). The
   integration suites run against the host when
   `GITPULSE_TEST_TERMINAL_BACKEND=host`; CI runs them both ways.
3. **Not in this change:** make the host the default and delete the in-process
   backend (the app constructing the engine itself) in the same commit. Blocked
   on the security sign-off above.
