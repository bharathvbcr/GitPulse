# File-claim cwd experiment

Recorded 2026-11-05 21:16 local (2026-11-06T03:16:11Z) on this host.
The question is which directory an EnterWorktree session should be attributed
to: the process cwd (`lsof`), the registry cwd, or the cwd the edit hook
reports. No EnterWorktree session was running, so this file separates what was
measured from a fixture that only reproduces that shape.

## Measured

### Live Claude Code CLI, pid 4616

`lsof -a -p 4616 -d cwd` reported cwd

`/Users/bharath/Code/devtools/GitPulse/.gitpulse/worktrees/set-up-claude-code-s-own-worktrees-throu-532d781d`.

The process is Claude Code `2.1.318`
(`$HOME/.local/share/claude/versions/2.1.318`), permission mode
`bypassPermissions`. GitPulse insights named the same pid as a `claude` cli
session `532d781d-f5bd-4d2c-87da-1dd432130744`, status waiting, in that lane.
The process cwd is the lane itself. An EnterWorktree session keeps its process
inside the parent checkout, so this process is not that shape. `lsof -p 4616`
showed no open registry json. `~/.claude/sessions` has no `4616.json`.

### Registry file that does exist

`~/.claude/sessions` contained one file, `95658.json`:

| field | value |
| --- | --- |
| pid | 95658 |
| cwd | `/Users/bharath/Code` |
| procStart | `Thu Nov  5 17:58:03 2026` |
| sessionId | `dcc64f45-fd82-45b2-bb44-14e517585e20` |
| entrypoint | `claude-desktop` |
| version | `2.1.317` |
| status | idle |
| kind | interactive |

That is a desktop session in `/Users/bharath/Code`. It is a different process
from pid 4616, and it is not an EnterWorktree lane.

### Lock files

A walk of `/Users/bharath/Code/devtools/GitPulse/.git/worktrees` and
`/Users/bharath/Code/devtools/GitPulse/.gitpulse` found no file named `locked`.
There is no live lock whose pid can be compared with `lsof`.

### Lock text the 2.1.318 binary writes

`rg` over the `2.1.318` binary found the writer in `Zkn`:

```text
claude ${s} ${r} (pid ${process.pid} start ${h})
```

when `procStart` `h` is present, and otherwise

```text
claude ${s} ${r} (pid ${process.pid})
```

The same binary matches that line with

```text
^claude (?:agent|session) .{1,255} \(pid (\d{1,10})(?: start (.{1,255}))?\)$
```

No such line was read from a lock file in this run.

## Not measured

No live EnterWorktree session was observable, so there is no measured triple
of `lsof` cwd, registry cwd, and hook payload cwd for one EnterWorktree
session. Those three numbers are not invented here.

## Fixture, reproducing the EnterWorktree shape

`src-tauri/src/insights/live_sessions.rs` tests build the shape the finding
describes. They are a reproduction, not an observation of pid 4616 or of
`95658.json`.

- `a_locked_lane_owns_the_session_whose_process_cwd_is_the_main_checkout`:
  process cwd is a main checkout, and the lane's `locked` file is
  `claude agent lane-1 (pid 42 start Thu Nov  5 17:58:03 2026)`. The start
  string is the desktop registry's `procStart`, copied as fixture text. With
  no lock, a claim whose cwd is main attributes the pid to main. With the
  lock, the same pid is attributed to the lane.
- `a_lock_without_a_start_does_not_steal_a_session`: a pid-only
  `claude session … (pid 42)` line is parsed and then ignored, so the session
  stays on the process cwd.
- `a_hook_reported_worktree_wins_when_registry_cwd_differs_from_process_cwd`:
  process cwd is the main checkout, registry cwd is a third directory, and
  `record_observed` writes the lane path as the hook cwd. Attribution follows
  the hook cwd.

`cargo test --manifest-path src-tauri/Cargo.toml -p gitpulse --lib --offline`
ran these three with the rest of `insights::live_sessions` in this session,
before this note (note clock 2026-11-06T03:16:11Z). 37 tests passed, 0 failed,
including those three.

## What remains

A person still has to run one EnterWorktree session and compare, on that same
session, `lsof` cwd, the registry cwd, and the cwd the edit hook records.
Until that comparison exists, the attribution rule above is covered by the
fixture and by the binary's lock template, not by a live three-way measurement.
