/**
 * Words for the dock's session toolbar and the cross-repository list.
 *
 * The toolbar's two numbers are the registry length and the session limit:
 * sessions open, then how many may be open at once. Grouping stays in
 * `checkoutLabel.groupSessionsByRepository`. Idle is not a field on the
 * record; a status is called idle only when the registry itself says so.
 */

import { DETACHED_STATUS, ENDING_STATUS } from "./detachedSessions";

/** Visible toolbar text. Not `0/32`: each number says what it counts. */
export function sessionCountLabel(open: number, limit: number): string {
  const sessions = Number.isFinite(open) ? Math.max(0, Math.trunc(open)) : 0;
  const cap = Number.isFinite(limit) ? Math.max(0, Math.trunc(limit)) : 0;
  return `${sessions} open / ${cap} limit`;
}

export function sessionStatusLabel(status: string): string {
  switch (status) {
    case "running":
      return "Running";
    case DETACHED_STATUS:
    case ENDING_STATUS:
      return status;
    case "starting":
      return "Starting";
    case "closing":
      return "Closing";
    case "exited":
      return "Exited";
    case "error":
      return "Error";
    default: {
      const trimmed = status.trim();
      if (trimmed.toLowerCase() === "idle") return "Idle";
      return trimmed || "Unknown";
    }
  }
}

/** A session that still holds a process, from the status string alone. */
export function sessionIsLive(status: string): boolean {
  return (
    status === "running" ||
    status === "starting" ||
    status === "closing" ||
    status === DETACHED_STATUS ||
    status === ENDING_STATUS
  );
}

export function sessionListNotice(total: number, shown: number, live: number): string | null {
  if (total <= 0) return "No terminal sessions are open.";
  if (shown <= 0) return "No sessions match.";
  if (live <= 0) return "No session is running.";
  return null;
}

export type SessionListCommand = "up" | "down" | "enter" | "escape" | "none";

export function sessionListCommand(key: string): SessionListCommand {
  if (key === "ArrowDown") return "down";
  if (key === "ArrowUp") return "up";
  if (key === "Enter") return "enter";
  if (key === "Escape") return "escape";
  return "none";
}
