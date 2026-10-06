# Terminal

Open the dock with **Ctrl+`**. It stays below the current view; **Expand terminal**
gives it as much of the window as will fit (up to 2400px) and **Restore terminal
size** returns to its previous height. Dragging the splitter can also grow past
the old 900px cap.

**The dock belongs to one repository tab.** Opening a terminal in one repository
does not open it in the others, and switching to a repository whose terminal you
have not opened shows that repository whole. Because hosting a terminal panel
starts a shell, this is also what stops a walk through several repositories from
spawning a shell in each. Each repository remembers whether its dock was showing,
including across a restart, and its dock reappears when you return to it.

Hiding the dock or switching repositories preserves its shells. Closing a terminal
tab terminates that session. Closing a repository tab closes its terminal sessions,
and GitPulse asks first when there are any — **Close Other Tabs** and **Close Tabs
to the Right** count every repository they would discard.

A Manvi session is the wrap around DevCouncil components (policy, agent hosting). GitPulse uses it here without requiring the rest of the DevCouncil suite.

Choose a session type in **New session type** (searchable), then press **+**. The
command palette also names each one directly — *New Terminal Session*, *New Claude
Session*, and so on — which opens this repository's dock if it is hidden and starts
the session in one step. Each repository keeps its own tabs. The tab strip scrolls
horizontally — including with a vertical wheel — and compresses labels once more
than four tabs are open.

**Find terminal session** searches this repository's tabs. **All terminal sessions**
searches across repositories and shows each session's repository, launcher, and
status. **Go to** brings a session on screen wherever it is running, switching
repository tabs and opening that repository's dock when the session lives in
another one. A session working on a task — a task terminal, or one adopted after a
reload — also offers **Task**, which opens that task on the Tasks board; an ended
task tab offers **Open task** beside its disabled Restart, since a new attempt
starts from the task. The link reads the task from the attempt's run record, the
one place that names it. **Close session** frees capacity. A failed close stays
listed with an explicit retry; it does not silently release a live process.

A repository tab carries a terminal glyph while it holds live sessions, with a count
past the first, so a shell running in a repository you are not looking at is still
visible — and so the answer to "what is using every slot" is on screen.

How many sessions may be open at once across repositories, including starts
and closes still in flight, is **Settings → Agents → Terminal sessions open at
once**: 32 until you change it, up to 128. The toolbar's session count shows
it (`12/32`). A change applies to the next session; lowering it closes
nothing, and new sessions wait until enough close. The ceiling bounds each
session's PTY pair, reader thread and 256 KiB output window, and stays well
inside macOS's system-wide PTY allowance, which other terminals share.

Use **Terminal tab options** to rename a tab, move it left or right, copy selected
text, copy retained output, or export retained output through a native save dialog.
An empty custom name restores the program's automatic title. Inactive tabs mark
unread output; ended and failed sessions remain inspectable and can be restarted.

**Split terminal** displays two sessions without restarting them. Wide panels put
them side by side; narrow panels stack them. Short docks scroll their panes so
Find and the terminal grid remain usable. **Close split view** preserves both
sessions. Selecting a third tab replaces the second pane.

**Find** searches the active session's retained output, including wrapped lines and
Unicode text. It supports next/previous matches and case sensitivity. Search is
literal, limited to 256 characters, and retains the existing 5,000-line scrollback.
At 1,000 highlighted results the UI reports **1000+ matches**, not an exact total.

Use **Latest output** to return from scrollback. **Clear scrollback** retains the
current prompt and session. Text-size controls in the footer change that terminal
from 10px to 24px; click the size to reset it to 12px. The chosen size becomes the
default for future sessions. The new-session launcher and dock height are also
saved. Shell processes, tab names, and scrollback are not restored after app exit.

From **Coverage**, select **Claude Code** or **Codex** and use **Generate coverage**
or **Improve coverage** to open a new agent session in that repository. **Preview
prompt** shows the exact task, detected languages, accepted report paths and
current coverage snapshot; **Copy agent prompt** lets you use an existing session.
These actions remain available without coverage or after a scan failure. The
agent uses its configured permissions and may ask for login or approval in the
terminal. **View agent session** returns to that tab, including its finished
transcript; **Rescan results** checks the artifacts after the run.
See [coverage generation](COVERAGE.md) for details and verification.

Saved tasks can also launch a dedicated terminal session from their run controls.
GitPulse binds the launch to the saved brief, selected checkout and a single run
attempt. Reconnecting a tab keeps the same live process and its scrollback; ended attempts
need an explicit new launch from task details. Process exit does not accept the
task; the agent marks it Done or Review itself through the GitPulse MCP server.

A task's agent gets the same notification flags as an agent tab (*Configure
agent CLIs GitPulse launches* in the session notification settings), so one waiting on a permission prompt in a
hidden tab still raises an alert. Which of Claude Code's settings files an
agent loads is **Settings → Agents → Claude Code settings files** (user,
project, local; all three — Claude Code's own default — until you change it).
It applies to task attempts and new Claude tabs alike, as `--setting-sources`.
Turning off project and local stops a repository's `.claude/settings*.json`
from widening an agent's permissions, and also drops that repository's
allow-lists and hooks. Managed sessions always load your user settings only;
that is Manvi's rule, since project settings could answer the approvals the
managed lane exists to show you.

**Settings → Agents → Agent models** chooses which model each agent starts
with, for task attempts and new agent tabs alike; empty means the CLI's own
choice. Claude Code takes a model (an alias such as `opus`, `opusplan` or
`sonnet[1m]`, or a full model name), an effort level, up to four fallback
models (`--fallback-model`) and an advisor model (the `advisorModel` setting,
passed in the same session-only `--settings` object as the notification
channel — Claude Code's `--advisor` flag is hidden from its `--help`, and a
task launch only passes what the installed build can be shown to support).
Antigravity takes a model (a slug from `agy models`) and an effort level;
Codex and Grok take a model only, because neither lists the effort values it
accepts. A task attempt checks that the installed CLI advertises each control
and the chosen effort level before starting. GitPulse checks that a model name
is well-formed, not that it exists: the CLI reports one it cannot use, except
Antigravity, which falls back to its default model with a warning. Managed
sessions are not affected; Manvi starts those with the CLI's own model.

The model fields suggest names from each CLI where it has them.
**List models** on the Antigravity row runs `agy models` (from the temporary
directory, never the repository; it asks Antigravity's service under your
sign-in, so it is only run when you press the button) and offers that account's
models; the answer is reused for ten minutes unless you press **Refresh
models**, and a stored model the list does not contain is flagged. Claude Code
publishes no model list — it has no command for one, and the API's list needs
credentials GitPulse does not read — so the Claude row offers its built-in
aliases plus the models your `~/.claude/settings.json` names (`model`,
`advisorModel`, `availableModels`). Codex has no model list, and `grok models`
talks to a background process it may start, so neither is asked. A listing
that fails says why and keeps the last list it had.

Claude Code is also given the attempt's id as
its session id and the brief's private folder as a readable directory. An ended
Claude Code attempt offers **Resume conversation**: GitPulse looks for the
transcript Claude Code saved for that session and, if it is there, opens a new
Claude Code tab in the attempt's checkout with `claude --resume`, in the
attempt's own permission mode. If there is no saved conversation (attempts
launched before this, or one Claude never got as far as saving), or its checkout
has been removed, it says so instead of opening a tab. These handoffs remain user-controlled CLI sessions; structured managed-run
questions and approvals use the separate Manvi protocol. See
[Tasks and workspaces](TASKS_AND_WORKSPACES.md) for copying, permissions and
verification limits.

If the window reloads, the terminal processes it started keep running: they are
detached rather than stopped, so one that prints a lot is no longer killed for
lack of a page to show it. The reloaded window lists them under **Sessions** as
still running, counts them against the session limit, and can stop them or
show one again with **Go to**, which takes the same process over in a new tab
(a task attempt's comes back as its own task tab). Output printed while the
window was reloading is not kept, so the new tab starts empty; a full-screen
program such as Claude Code is made to repaint, and a shell shows its next
prompt.

| Shortcut | Action |
| --- | --- |
| Ctrl+Shift+T / Ctrl+Shift+W | New shell / close current session |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous terminal tab |
| Left / Right / Home / End on a focused tab | Navigate the tab strip |
| Command+F or Ctrl+Shift+F | Find in active terminal |
| Enter / Shift+Enter in Find | Next / previous result |
| Escape in Find | Close Find and focus the terminal |
| Command + / − / 0 (Ctrl+Shift on Windows/Linux) | Increase / decrease / reset text size |
| Up / Down on the dock separator | Resize the dock |

Holding the new/close shortcut does not repeatedly create or terminate shells.
IME composition and the shell's ordinary Ctrl+F binding are preserved.

## Input, output, and recovery

Typing and paste are delivered in order in chunks of at most 16 KiB. The pending
input limit is 1 MiB; a paste that would exceed it is refused in full with a visible
message. Invalid Unicode is also refused before any portion is sent. Failed or
timed-out writes are not retried, since part of a command may already have arrived;
restart the session before entering more input.

The native PTY pauses after 256 KiB of output awaiting renderer acknowledgements.
If the renderer stops acknowledging for 30 seconds, the session closes with a
delivery error. This bounds output in transit; retained history is still limited
to 5,000 scrollback lines. Copy/export includes that retained buffer, not output
already evicted. Exports are limited to 16 MiB.

Listener attachment, input, resize, and close requests have five-second frontend
deadlines. A spawn still unresolved after 15 seconds retains its capacity slot;
a late process is closed before another can start. Restart waits for confirmed
native cleanup and drains the old renderer before showing the replacement.

A live session reports what it is running: its foreground program, whether that
program is a job under the shell, and its working directory. Paste and erase guards
use that report instead of predicting what the terminal handled, and a new terminal
can start in a chosen directory. Ctrl+K typed into a terminal is the shell's
kill-line; the command palette leaves it alone there.

Interactive shells remain user-controlled and run outside the Manvi wrap.
Console uses the existing direct-command execution path, with bounded output,
timeouts, and Manvi gating for Git commands. It retains up to 100 commands and
100 results / 8 MiB, and discloses removed results. Completion preserves the
reader's scroll position and does not steal focus from another control.

## Links in output

URLs and repository file references in terminal output are clickable. Hovering
one names its target in the session footer before you commit to the click, so
the text on screen is never the only evidence of where it goes.

Two rules bound what a click can do, and both fail closed. A URL opens in your
browser only when its scheme is `http` or `https`; every other scheme —
`file:`, `javascript:`, `data:`, or an app handler such as `vscode:` — is not
underlined, not hoverable and not openable. A file reference opens in the code
viewer only when it resolves inside the session's repository; an absolute path
elsewhere on disk, or one that climbs out with `..`, is not a link either.
Terminal output is untrusted text, so a span GitPulse will not open is never
decorated as though it would.

`path:line:column` opens the file in the code viewer and lands on that line,
with the line selected. A reference naming a line past the end of the file
opens near the end rather than refusing. The request is dropped if nothing
collects it within 30 seconds, so it cannot fire later when the same file is
opened for an unrelated reason.

Hyperlinks a program embeds itself (OSC 8) obey the same allowlist, checked
against the escape sequence's target rather than its display text.

## Screen reader support

**Settings → Appearance → Terminal screen reader support** builds xterm's
accessible row tree and announces new output. It is off by default because the
tree is rebuilt as output arrives, which costs time on a session that prints
continuously; turning it on applies to sessions already running.

It does not make the shell's input line an editable accessible field. The
focused element in a terminal is xterm's helper textarea, whose value is empty
by design — what you type goes straight to the PTY and is painted as grid
cells. Text-expansion utilities that work by reading the focused field and
rewriting it therefore cannot act inside the terminal, and will report that the
text they expected to find is not there. They work normally in **Console** and
in the rest of the application, which use ordinary input fields.

## Frontend verification

`bun run test:browser -- --harness terminal` runs the harness in headless
Chrome and fails the run on any failed or missing assertion; `--all` derives
the list from `BROWSER_HARNESSES`, and `--webkit` runs the same page in
WKWebView. For local work, `bun run dev` and `/harness/terminal.html` still
offer **Run terminal checks** and **Run input stress checks** as buttons.

The harness mounts the real dock, panels, sessions,
and xterm renderer with Tauri's official mock transport. It checks link
detection, hover targets, refused schemes and paths, search,
keyboard focus, global tab limits, session preservation, split panes, resizing,
narrow/light layout, and exact large-paste delivery.
The displayed counters make shell writes, spawns, kills, and runtime errors visible.
This does not validate native PTY execution; no commands execute in the harness.
See [the archived terminal audit](archive/TERMINAL_AUDIT.md) for real-PTY tests, failure evidence,
ownership contracts, and remaining platform verification limits.
