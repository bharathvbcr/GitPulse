# Terminal

Open the dock with **Ctrl+`**. It stays below the current view; **Expand terminal**
gives it more room and **Restore terminal size** returns to its previous height.
Hiding the dock or switching repositories preserves its shells. Closing a terminal
tab terminates that session; closing a repository tab closes its terminal sessions.

A Manvi session is the wrap around DevCouncil components (policy, agent hosting). GitPulse uses it here without requiring the rest of the DevCouncil suite.

Choose Shell, Claude, Manvi, or Codex in **New session type**, then press **+**.
Each repository keeps its own tabs. The tab strip scrolls to the selected tab.
The application allows 16 concurrent sessions across repositories, including
starts and closes still in flight. **All terminal sessions** shows their repository,
launcher, and status, and lets you close a session to free capacity. A failed close
stays listed with an explicit retry; it does not silently release a live process.

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
attempt. Reattaching keeps the same live process and scrollback; ended attempts
need an explicit new launch from task details. Process exit does not accept the
task. These handoffs remain user-controlled CLI sessions; structured managed-run
questions and approvals use the separate Manvi protocol. See
[Tasks and workspaces](TASKS_AND_WORKSPACES.md) for copying, permissions and
verification limits.

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

Interactive shells remain user-controlled and run outside the Manvi wrap.
Console uses the existing direct-command execution path, with bounded output,
timeouts, and Manvi gating for Git commands. It retains up to 100 commands and
100 results / 8 MiB, and discloses removed results. Completion preserves the
reader's scroll position and does not steal focus from another control.

## Frontend verification

Run `npm run dev`, open `/harness/terminal.html` on the reported local URL, then
click **Run terminal checks**, then **Run input stress checks**. The harness mounts the real dock, panels, sessions,
and xterm renderer with Tauri's official mock transport. It checks search,
keyboard focus, global tab limits, session preservation, split panes, resizing,
narrow/light layout, and exact large-paste delivery.
The displayed counters make shell writes, spawns, kills, and runtime errors visible.
This does not validate native PTY execution; no commands execute in the harness.
See [the archived terminal audit](archive/TERMINAL_AUDIT.md) for real-PTY tests, failure evidence,
ownership contracts, and remaining platform verification limits.
