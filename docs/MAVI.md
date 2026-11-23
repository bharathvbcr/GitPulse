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
  `--resume`, and shows the earlier turns read from Claude Code's transcript (see
  below).

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

## Mavi and the rest of GitPulse

Mavi starts with the app (after the first paint), so these work before the
Mavi view is ever opened.

| Where | What it does |
| --- | --- |
| Header | The Mavi view tab carries an amber count of threads waiting for your answer, or a red dot for one that failed and that you have not looked at. |
| Toasts | A thread that starts waiting for an answer, or fails, says so with **Open**, unless it is on screen in a focused window. Only changes seen while GitPulse runs are announced (a reload is quiet), a finished turn is not announced, and the same request is not repeated within a minute. |
| Command palette | **New Mavi thread**, **Ask about the open file in Mavi** (starts a thread with `@path`), and **Open Mavi thread: …** for up to 12 threads, most urgent first. |
| Settings → Agents | A new thread starts with Claude Code's default permission mode and model from Settings. Bypass still needs its acknowledgement in the form. |
| Worktrees | **Run in a new worktree** creates a sibling `<repo>.wt-<slug>` on a `mavi/<slug>` branch, the same way the dock's isolated-agent preset does. It is ticked for you when this repository already has a live thread, which the host would refuse. The form warns when an agent terminal already works in the checkout, since its edits would show as Other changes. |
| Explorer | Live pulse shows which thread is editing the repository (see above). |
| Quit | Threads in the middle of a turn are listed in the same "Quit while agents are running?" question as terminal agents, whatever the terminal backend keeps running, because Mavi agents always end with the app. |

### The terminal dock

- **New Mavi thread** in the dock's **+** menu. Mavi is not a session kind; nothing starts in the dock.
- **The terminal pill's menu lists live Mavi threads** beside terminal sessions, with the same status words as the rail.
- **Files a Mavi agent changes reach the dock** the way a terminal agent's do (`agentEvents.dispatchTouchedFile`): open file panes showing them reload, and **Follow agent** can show them. This covers each applied edit and, at the end of a turn, the Other changes.
- **Open a terminal here** (thread header): a shell in the thread's worktree, in the dock.
- **Continue in terminal** (thread header, local threads with a saved conversation): moves the conversation to a Claude Code tab in the dock, resuming it with the thread's permission mode. **Bring back to Mavi** moves it back once the terminal session has ended.

**One process per conversation.** A Mavi agent and a terminal tab resuming the
same conversation would both write to one Claude Code transcript. So:

- the move is offered only between turns, with nothing queued, so there is no interrupt to race and no queued message to lose;
- before anything is stopped it checks that the agent saved the conversation, that no terminal tab already has it (if one does, it shows that tab), that the dock has room, and that the repository opens as a tab;
- then the Mavi agent ends and the move waits until it has exited (up to 5 s) before the terminal tab starts. A failure after that point says the conversation is intact and can be resumed in Mavi;
- Mavi refuses to resume a conversation a terminal tab is still running (and shows that tab), and the host refuses to start a second Mavi thread on a conversation another thread already runs;
- the moved state is saved with the thread, so a reload still shows "This conversation continues in a terminal tab".

- **Continue in Mavi** (a Claude Code tab's menu in the dock): moves the tab's conversation into a Mavi thread, with its earlier turns. Before the tab's session ends it checks that the conversation is saved for this checkout (not a subdirectory or another worktree, where Claude Code alone can resume it), that no Mavi thread already works in that worktree, and that Mavi has room; then it asks (Bypass is named in the question), ends the tab's process, waits until it has exited, and only then starts the Mavi agent. A conversation that was a Mavi thread before it moved goes back to that thread.

SSH threads cannot move to the local dock, and the Agents window does not list
Mavi threads yet (its rows come from terminals, worktrees and task runs), so it
undercounts running agents while Mavi threads run.

### Earlier turns, from the transcript

Whenever Mavi resumes a conversation (Resume after a quit, Bring back to Mavi,
Continue in Mavi) the host reads Claude Code's own transcript for it and seeds
the thread with the earlier turns, so they render exactly like live ones: your
prompts, the agent's prose and thinking, tool rows, edits with their exact
hunks (the Changes column's Thread scope includes them), interruptions, and a
note where a background task reported back. A marker says how many turns came
from the transcript and where Mavi takes over. What a transcript does not
record per turn (duration, cost, the turn-end summary) is not invented: those
turns read "Earlier turn".

- Read from `<config>/projects/<encoded cwd>/<id>.jsonl`, found by the same
  locator GitPulse uses for terminal resumes, across every Claude Code config
  directory it knows.
- Bounded: the last 16 MiB of the file and the last 4,000 timeline lines; a
  cut is announced.
- Each line is slimmed and redacted exactly as a live line is.
- Dropped: subagent side-chains, attachments, queue and cost bookkeeping,
  slash-command plumbing. A turn Claude Code started itself (a background task
  reporting back) is shown as a note, not as your message.
- A conversation saved for another directory is refused rather than resumed
  empty: Claude Code resumes a conversation only from where it was saved.
- On an SSH host the transcript stays on the host; the thread says so and
  starts at the resume.

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
and a remote daemon that keeps the agent alive across disconnects.

## Design sources

The design follows an audit of T3 Code (MIT) and Herdr (Apache-2.0; its
reviewer and sidebar plugins are MIT) for their chat and SSH models; no code
was copied. Patterns adopted: one typed item model behind the timeline, tool
runs collapsed by verb with failures surviving collapse, approvals as items
answered in place, a snapshot-plus-cursor replay, a priority-ordered thread
status, a per-turn baseline taken at the host's own turn start, attribution
per tool call with an explicit bucket for everything else, and gating any
send-back on the agent's state.
