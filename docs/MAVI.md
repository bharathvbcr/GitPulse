# Mavi: agent conversations

Mavi is GitPulse's chat-first surface for coding agents. Every other agent
GitPulse hosts is a terminal; Mavi runs Claude Code in its structured mode and
renders what the agent does as a timeline, with permission requests answered in
place and a live view of what each turn changed. It runs on this computer or on
another machine over SSH.

Mavi is not Manvi. Manvi is the policy tool; Mavi is a view.

## Using it

Open **Mavi** in the header (⌥⌘4 on macOS, Ctrl+Shift+4 elsewhere).

- **New thread**: choose where the agent runs (this repository, or an SSH host
  and a directory on it), what it may do (the same permission modes as agent
  terminals, least authority first; **Ask first** is the default), and the
  first message.
- **The conversation**: your messages are boxed; the agent's prose is not.
  Runs of tool calls collapse into one row that says what they did
  ("Read 3 files · Edited 2 files · Ran 1 command"). Failed or denied calls
  are counted on the row even when it is collapsed, and edited files are always
  shown as chips. The call running now is shown live.
- **Permission requests** appear where they happen, with what the agent wants
  to run: the command, or for an edit the change itself, built from the
  request. **Allow** (A) or **Deny** (D, with an optional note for the agent).
  The keys work only while the card has focus. When the card is off screen, a
  bar above the composer says the agent is waiting.
- **The composer**: Enter sends, Shift+Enter is a new line. While a turn runs,
  what you send waits in a visible queue and goes as one turn when the agent is
  free. **Stop** (or Esc from an empty composer) interrupts the turn.
- **Changes** (right column): what the selected turn, or the whole thread,
  changed, file by file. See below.
- **Resume**: quitting GitPulse ends every agent. A thread keeps the
  conversation id, and **Resume** continues the conversation with
  `--resume`. The earlier timeline is not restored (it is not persisted; the
  agent's own transcript holds the conversation) and the view says so.

## The live changes view

Each file in the Changes column says where its change came from:

| State | Source | What is shown |
| --- | --- | --- |
| **Waiting for you** | an edit the agent is asking to make | the proposed change, built from the request's own input, before it lands |
| **Applying** | an edit call that has not returned | the change from its input |
| (applied) | an edit the agent made with its own tools | the exact hunks Claude Code reported (`structuredPatch`), per call, with line numbers |
| **Denied** / **Failed** | a refused or failed edit | listed so a refusal is visible; not counted in +/− |
| **Other** | a path that moved during the turn that no edit tool names | its patch against `HEAD`, read when opened |

"Other" catches what an agent's edit tools never report: a command it ran, a
formatter, a code generator, a test run updating a snapshot, or another program.
The view never claims those were the agent's.

**How "Other" is found.** When a turn starts the host takes a fingerprint of
the worktree (the porcelain status of every changed path plus each file's size
and modification time) and takes another when the turn ends (or the agent
exits). Paths whose state moved are the turn's changes. A fingerprint is
read-only and kept in memory: no commit, no ref, no file contents. This is
deliberately not a turn checkpoint (`engine::turn_checkpoint`), which writes
file contents under a hidden ref and is off unless the person turns it on.

Limits: two edits within one filesystem timestamp tick that leave the size
unchanged read as one state (git's own racy-clean caveat); a worktree with more
than 5,000 changed paths is compared partially and the turn says so; changes
made after a turn's `result` by a background task the agent left running land in
the next turn.

**In Explorer.** The Live pulse column now shows which Mavi thread is editing
this repository, badges the files it touched ("Agent editing"), lists those
first while it works, and warns before **Discard** on a file an agent is
editing right now.

## How it works

```
Mavi view (Svelte)                       Rust host (src-tauri/src/mavi)
  store.svelte.ts ── cmd_mavi_* ──────▶  Session: child process, 4 threads
  reduce.ts ◀── mavi-event (batched) ──    writer  ─ stdin  ─▶ claude -p --input-format stream-json
  rows.ts / changes.ts                     reader  ◀ stdout ─  --output-format stream-json
  ◀── mavi-status (summary) ─────────     processor: prepare → ring → emit (40 ms batches)
                                           local:  claude in the worktree
                                           ssh:    ssh -T … -- 'exec sh -c "cd DIR && exec claude …"'
```

- **Wire.** Claude Code 2.1.330's bidirectional NDJSON mode with
  `--permission-prompt-tool stdio`: permission requests arrive as
  `control_request/can_use_tool` and are answered with `control_response`;
  interrupts are `control_request/interrupt`. Fixtures captured from the real
  CLI live in `src/lib/mavi/__fixtures__/`.
- **Host lines.** The host adds its own lines (`type: mavi_*`): the user's
  turn, answers, interrupts, the turn's moved paths, stderr, exit, and markers
  for cut or withheld lines.
- **Replay.** Finished lines are kept in a bounded, memory-only ring
  (24 MiB / 20,000 lines). Streaming deltas are not kept: each block also
  arrives whole. A page that mounts or reloads replays from a sequence cursor
  (`cmd_mavi_events`) and then follows live.
- **Slimming and redaction.** Before a line reaches the page or the ring, the
  host drops what the page never reads (the 35 KB tool catalogue `init`
  repeats every turn, an edit's whole `originalFile`, a Read's duplicate
  content), cuts tool output at 32 KiB, and redacts credential shapes with the
  same detector the commit leak scan uses.
- **Status without replay.** The host keeps a summary per thread (phase,
  pending approvals, cost, files touched, background tasks) and emits it on
  change, so the rail and Live pulse never replay threads they are not showing.

## Safety

- **Permission mode** is expanded only by the shared policy table
  (`apply_permission_mode`), so **Bypass** needs the same acknowledgement as an
  agent terminal and the default is **Ask first**.
- **Answers are the host's.** The page sends a request id and allow or deny.
  The host echoes back the input the agent proposed, from the request it
  received, so what was shown is what runs. There is no "allow for the session":
  the rule Claude Code suggests with a request can raise the whole session's mode
  (an Edit suggests `acceptEdits`), and that choice belongs to the thread's
  mode.
- **Repository trust** is required for a local thread. **One live local thread
  per worktree**: two agents in one checkout would make every "what this turn
  changed" answer a mix of both; use a worktree.
- **Process lifetime.** Each agent runs in its own process group; ending a
  thread (or quitting) terminates the group, then kills it after 3 s.

### SSH

The system `ssh` binary, so your `~/.ssh/config` (aliases, users, ports, jump
hosts, `ControlMaster`), agent and `known_hosts` apply. No daemon, port or token:
the agent's stdio *is* the transport.

- `BatchMode=yes`: ssh never prompts. A host that needs a password says so
  instead of hanging.
- `StrictHostKeyChecking=yes`: an unknown or changed host key is refused, never
  accepted for you. Connect once from a terminal to confirm a new host.
- `ClearAllForwardings=yes`, `ServerAliveInterval=15`, `ConnectTimeout=10`.
- The destination is validated (letters, digits, `.`, `_`, `-`, one `@`; never
  a leading `-`) and placed after `--`, so it can never be read as an option.
  The directory and every argument are single-quoted, and the login shell only
  ever sees `exec sh -c '<script>'`, so fish or csh quoting rules do not apply.
- **Check connection** returns a typed answer: ready, sign-in failed, host key
  unknown or changed, unreachable, no such directory, Claude Code missing, not a
  Git repository, or timed out.

Limits of the SSH path today: the agent stops when the connection drops (resume
continues the conversation from the host's transcript); a remote thread has no
fingerprint, so its change view is the agent's own edits only, and a file's
patch against `HEAD` is read over ssh on request. The SSH path is covered by
unit tests and a shell round trip of the exact remote command; it has not been
exercised against a live sshd in CI.

## Not yet

Codex (`codex app-server`), image attachments, restoring files to a turn,
a remote daemon that keeps the agent alive across disconnects, and persisting
the timeline across app restarts.

## Design sources

The design follows an audit of T3 Code (MIT) and Herdr (Apache-2.0; its
reviewer and sidebar plugins are MIT) for their chat and SSH models; no code
was copied. Patterns adopted: one typed item model behind the timeline, tool
runs collapsed by verb with failures surviving collapse, approvals as items
answered in place, a snapshot-plus-cursor replay, a priority-ordered thread
status, a per-turn baseline taken at the host's own turn start, attribution
per tool call with an explicit bucket for everything else, and gating any
send-back on the agent's state.
