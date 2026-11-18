/**
 * What a row of the Agents plane offers to do, decided once.
 *
 * Every row used to get "Show terminal" and "Open task" only when it had a
 * terminal or a run behind it, so an agent checkout left on disk — the most
 * common row — offered nothing but "Open checkout", and a child process row
 * whose parent sat outside every checkout offered nothing at all. Here each
 * row gets the moves that make sense for what it is:
 *
 * - a row with a terminal or a run: show it, open its task (unchanged);
 * - an agent checkout with nothing of this window in it: start an agent there,
 *   with the agent its layout names when GitPulse can launch that agent;
 * - a checkout with uncommitted work: review it;
 * - a child process: its parent's terminal and task, named as the parent's,
 *   because nothing here drives a process this window did not start.
 *
 * The view renders the list and routes each kind to the owner of that move.
 * Removing a checkout is not here: that is destructive and the Worktrees
 * panel owns its force rules.
 */

import { isPromptLauncher } from "../terminal/launchRequests";
import type { SessionKind } from "../terminal/tabs";
import { agentKindLabel } from "../work/agentWorktree";
import { plural } from "../format";
import type { AgentRow } from "./plane";

export type AgentRowActionKind = "terminal" | "task" | "start" | "review" | "checkout";

export interface AgentRowAction {
  kind: AgentRowActionKind;
  /** Visible button text. */
  label: string;
  /** Accessible name: says which row it acts on. */
  ariaLabel: string;
  /** Hover text: what the click does and what it does not. */
  title: string;
  /** The row whose terminal, run or checkout the action uses: the row itself, or a child's parent. */
  target: AgentRow;
  /** For `start`: the launcher the new terminal runs. */
  launcher?: SessionKind;
}

/** The launcher a start runs: the agent the checkout's layout names when GitPulse can launch it, else the dock's default. */
export function startLauncher(row: AgentRow, defaultLauncher: SessionKind): SessionKind {
  const kind = row.kind.trim().toLowerCase();
  if (isPromptLauncher(kind)) return kind;
  return defaultLauncher === "browser" ? "shell" : defaultLauncher;
}

function startAction(row: AgentRow, defaultLauncher: SessionKind): AgentRowAction {
  const launcher = startLauncher(row, defaultLauncher);
  const name = launcher === "shell" ? "" : agentKindLabel(launcher);
  const occupied = row.outsideSessions > 0;
  const label = launcher === "shell"
    ? "Open terminal"
    : occupied ? `Start another ${name}` : `Start ${name}`;
  const already = occupied
    ? ` ${plural(row.outsideSessions, "agent process", "agent processes")} started outside this window ${row.outsideSessions === 1 ? "is" : "are"} already running there.`
    : "";
  return {
    kind: "start",
    label,
    ariaLabel: `${label} in ${row.checkout} for ${row.session}`,
    title: `Opens ${row.checkoutPath} and starts ${launcher === "shell" ? "a shell" : name} in its terminal dock.${already}`,
    target: row,
    launcher,
  };
}

/**
 * The actions for `row`, in the order they are drawn. `parent` is the row a
 * child process sits under (null for a root); `defaultLauncher` is the dock's
 * remembered launcher, used when the checkout names no launchable agent.
 */
export function agentRowActions(
  row: AgentRow,
  parent: AgentRow | null,
  defaultLauncher: SessionKind,
): AgentRowAction[] {
  const actions: AgentRowAction[] = [];

  if (row.origin === "session") {
    // A child process: nothing here attaches to it, so the moves are its
    // parent's, said as the parent's.
    if (parent && (parent.liveKey || parent.taskRunId)) {
      actions.push({
        kind: "terminal",
        label: "Show parent terminal",
        ariaLabel: `Show the terminal of ${parent.session}, which ${row.session} runs under`,
        title: `Shows ${parent.session}. This child process has no terminal of its own here.`,
        target: parent,
      });
    }
    if (parent?.taskRunId) {
      actions.push({
        kind: "task",
        label: "Open parent task",
        ariaLabel: `Open the task of ${parent.session}, which ${row.session} runs under`,
        title: `Opens the task ${parent.session} runs for.`,
        target: parent,
      });
    }
  } else {
    if (row.liveKey || row.taskRunId) {
      actions.push({
        kind: "terminal",
        label: "Show terminal",
        ariaLabel: `Show terminal for ${row.session}`,
        title: row.liveKey ? "Brings this terminal on screen." : "Shows this run's terminal, starting it if it is not open here.",
        target: row,
      });
    }
    if (row.taskRunId) {
      actions.push({
        kind: "task",
        label: "Open task",
        ariaLabel: `Open task for ${row.session}`,
        title: "Opens the task this run belongs to.",
        target: row,
      });
    }
    // An agent checkout with no terminal and no run of this window: the row
    // that used to offer only "Open checkout".
    if (row.origin === "checkout" && !row.liveKey && !row.taskRunId && row.checkoutPath) {
      actions.push(startAction(row, defaultLauncher));
    }
  }

  if (row.origin !== "session" && row.checkoutPath && row.dirtyKnown && (row.dirtyFiles ?? 0) > 0) {
    const files = plural(row.dirtyFiles ?? 0, "change", "changes");
    actions.push({
      kind: "review",
      label: `Review ${files}`,
      ariaLabel: `Review ${files} in ${row.checkout} for ${row.session}`,
      title: `Opens ${row.checkoutPath} on its uncommitted changes.`,
      target: row,
    });
  }

  if (row.checkoutPath) {
    actions.push({
      kind: "checkout",
      label: "Open checkout",
      ariaLabel: `Open checkout ${row.checkout} for ${row.session}`,
      title: `Opens ${row.checkoutPath} as a repository tab. Starts nothing.`,
      target: row,
    });
  }
  return actions;
}
