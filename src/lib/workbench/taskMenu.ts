import { PRIORITY_LABELS } from "./boardDrag";
import { ARCHIVE_STATUS, archiveState } from "./taskArchive";
import { STATUSES, STATUS_LABELS, type TaskCard, type TaskStatus } from "./client";
import { displayTitle, isRevision, isTaskId } from "./taskDelete";
import type { AgentProvider } from "./vocabulary";

export type TaskMenuSubmenu = "move" | "priority" | "copy" | "due" | "owner" | "label" | "agent";

export type TaskMenuIcon =
  | "open"
  | "enhance"
  | "duplicate"
  | "copy"
  | "id"
  | "brief"
  | "move"
  | "priority"
  | "due"
  | "owner"
  | "label"
  | "agent"
  | "select"
  | "add"
  | "archive"
  | "delete";

/** Relative due targets the menu can set without opening the editor. */
export type TaskDueChoice = "today" | "tomorrow" | "next_week" | "clear";

/** How a handoff starts. Mirrors `RunKind` plus the provider it applies to. */
export type TaskAgentTarget =
  | { provider: AgentProvider; kind: "external_terminal" }
  | { provider: "codex"; kind: "managed" };

export type TaskMenuAction =
  | { kind: "open" }
  | { kind: "enhance" }
  | { kind: "duplicate" }
  | { kind: "copyTitle" }
  | { kind: "copyId" }
  | { kind: "copyBrief" }
  | { kind: "copyAgent" }
  | { kind: "move"; status: TaskStatus }
  | { kind: "priority"; priority: 0 | 1 | 2 | 3 }
  | { kind: "due"; choice: TaskDueChoice }
  | { kind: "owner"; owner: string | null }
  | { kind: "label"; label: string; add: boolean }
  | { kind: "agent"; target: TaskAgentTarget }
  | { kind: "selectColumn" }
  | { kind: "newInColumn"; status: TaskStatus }
  /**
   * File the selection in the archive.
   *
   * A verb of its own rather than another `move` row, because `Move to… ›
   * Done` is where a reader goes to change a *status* and the archive is
   * what they go looking for when they want the task off the board. The menu
   * had the first and not the second, so "archive this" had no answer
   * anywhere in the product. What it does is still exactly one status
   * change — `archiveAction()` owns that, not this menu.
   */
  | { kind: "archive" }
  | { kind: "delete" }
  | { kind: "submenu"; submenu: TaskMenuSubmenu };

export interface TaskMenuItem {
  id: string;
  label: string;
  action: TaskMenuAction;
  disabled?: boolean;
  danger?: boolean;
  /** Right-aligned note: a keyboard shortcut, or where the action runs. */
  hint?: string;
  separatorBefore?: boolean;
  icon?: TaskMenuIcon;
  group?: TaskMenuSubmenu;
  /**
   * Tri-state for a value item. `true` every selected task already has it,
   * `false` none does, `"mixed"` some do.
   *
   * Value submenus list every choice and mark the current one rather than
   * omitting it. Omitting made the menu change shape depending on the card
   * under the cursor, so the same gesture hit a different row each time — and
   * it left no way to see what a task's priority actually *was* without
   * opening it.
   */
  checked?: boolean | "mixed";
}

/** Owners and labels the board has actually loaded, offered as one-click values. */
export interface TaskMenuVocabulary {
  owners?: readonly string[];
  labels?: readonly string[];
}

/** How many loaded owners / labels the menu will list before stopping. */
export const MAX_MENU_VALUES = 12;

const DUE_CHOICES: readonly { choice: TaskDueChoice; label: string }[] = [
  { choice: "today", label: "Today" },
  { choice: "tomorrow", label: "Tomorrow" },
  { choice: "next_week", label: "Next week" },
  { choice: "clear", label: "No due date" },
];

const AGENT_TARGETS: readonly { id: string; label: string; hint: string; target: TaskAgentTarget }[] = [
  { id: "agent-claude", label: "Claude Code", hint: "Terminal", target: { provider: "claude", kind: "external_terminal" } },
  { id: "agent-codex", label: "Codex", hint: "Terminal", target: { provider: "codex", kind: "external_terminal" } },
  { id: "agent-grok", label: "Grok", hint: "Terminal", target: { provider: "grok", kind: "external_terminal" } },
  { id: "agent-agy", label: "Antigravity", hint: "Terminal", target: { provider: "agy", kind: "external_terminal" } },
  { id: "agent-codex-managed", label: "Codex", hint: "Managed", target: { provider: "codex", kind: "managed" } },
];

export function isContextMenuKey(event: { key?: unknown; shiftKey?: unknown }): boolean {
  return event.key === "ContextMenu" || (event.key === "F10" && event.shiftKey === true);
}

export function contextMenuAnchor(
  event: { clientX?: unknown; clientY?: unknown },
  rect: { left: number; top: number; width: number; height: number } | null,
): { x: number; y: number } {
  const x = typeof event.clientX === "number" && Number.isFinite(event.clientX) ? event.clientX : 0;
  const y = typeof event.clientY === "number" && Number.isFinite(event.clientY) ? event.clientY : 0;
  if (x > 0 && y > 0) return { x, y };
  if (!rect) return { x: 8, y: 8 };
  const left = Number.isFinite(rect.left) ? rect.left : 0;
  const top = Number.isFinite(rect.top) ? rect.top : 0;
  const width = Number.isFinite(rect.width) && rect.width > 0 ? rect.width : 0;
  const height = Number.isFinite(rect.height) && rect.height > 0 ? rect.height : 0;
  return { x: left + Math.min(width, 32), y: top + height };
}

export function toggleSelection(selected: ReadonlySet<string>, id: string): Set<string> {
  const next = new Set(selected);
  if (!isTaskId(id)) return next;
  if (next.has(id)) next.delete(id);
  else next.add(id);
  return next;
}

export function rangeSelect(
  orderedIds: readonly string[],
  from: string | null,
  to: string,
): Set<string> {
  const ids = orderedIds.filter(isTaskId);
  if (!isTaskId(to)) return new Set();
  const end = ids.indexOf(to);
  if (end < 0) return new Set();
  const start = from && isTaskId(from) ? ids.indexOf(from) : end;
  if (start < 0) return new Set([to]);
  const lo = Math.min(start, end);
  const hi = Math.max(start, end);
  return new Set(ids.slice(lo, hi + 1));
}

export function flattenVisibleIds(
  columns: Partial<Record<TaskStatus, { items: readonly TaskCard[] }>>,
  statuses: readonly TaskStatus[],
  visible: (card: TaskCard) => boolean,
): string[] {
  const ids: string[] = [];
  const seen = new Set<string>();
  for (const status of statuses) {
    for (const card of columns[status]?.items ?? []) {
      if (!isTaskId(card.id) || seen.has(card.id) || !visible(card)) continue;
      seen.add(card.id);
      ids.push(card.id);
    }
  }
  return ids;
}

export function cardsById(
  columns: Partial<Record<TaskStatus, { items: readonly TaskCard[] }>>,
  ids: ReadonlySet<string>,
): TaskCard[] {
  const out: TaskCard[] = [];
  const seen = new Set<string>();
  for (const page of Object.values(columns)) {
    for (const card of page?.items ?? []) {
      if (!ids.has(card.id) || seen.has(card.id) || !isTaskId(card.id) || !isRevision(card.revision)) continue;
      seen.add(card.id);
      out.push(card);
    }
  }
  return out;
}

export function duplicateTitle(title: unknown, cap = 300): string {
  const limit = Number.isSafeInteger(cap) && cap >= 16 ? cap : 300;
  const cleaned = displayTitle(title, limit);
  if (cleaned === "(untitled)") return "Untitled copy";
  const suffix = " (copy)";
  if (cleaned.length + suffix.length <= limit) return `${cleaned}${suffix}`;
  return `${cleaned.slice(0, limit - suffix.length)}${suffix}`;
}

export function menuPageItems(
  items: readonly TaskMenuItem[],
  page: "root" | TaskMenuSubmenu,
): TaskMenuItem[] {
  if (page === "root") return items.filter((item) => !item.group);
  return items.filter((item) => item.group === page);
}

export function submenuTitle(page: "root" | TaskMenuSubmenu): string {
  switch (page) {
    case "copy": return "Copy";
    case "move": return "Move to";
    case "priority": return "Priority";
    case "due": return "Due";
    case "owner": return "Owner";
    case "label": return "Labels";
    case "agent": return "Send to agent";
    default: return "Task actions";
  }
}

/**
 * Move keyboard focus by first letter, the way a native menu does.
 *
 * Returns the index to focus, or -1 when nothing matches. Wraps from the
 * current position so repeated presses of the same letter cycle through the
 * matches — which is the whole point on a Labels page with eight entries
 * beginning with the same word.
 */
export function typeAheadIndex(
  items: readonly { label: string; disabled?: boolean }[],
  query: string,
  from: number,
): number {
  const needle = query.trim().toLowerCase();
  if (!needle || !items.length) return -1;
  const start = Number.isInteger(from) && from >= 0 ? from : -1;
  for (let step = 1; step <= items.length; step += 1) {
    const index = (start + step + items.length) % items.length;
    const item = items[index];
    if (item.disabled) continue;
    if (item.label.trim().toLowerCase().startsWith(needle)) return index;
  }
  return -1;
}

/** true / false / "mixed" over the selection, for a value item's checkmark. */
function shared<T>(cards: readonly TaskCard[], read: (card: TaskCard) => T, value: T): boolean | "mixed" {
  if (!cards.length) return false;
  let some = false;
  let all = true;
  for (const card of cards) {
    if (read(card) === value) some = true;
    else all = false;
  }
  return all ? true : some ? "mixed" : false;
}

function labelState(cards: readonly TaskCard[], label: string): boolean | "mixed" {
  if (!cards.length) return false;
  const hits = cards.filter((card) => card.labels.includes(label)).length;
  return hits === cards.length ? true : hits > 0 ? "mixed" : false;
}

function bounded(values: readonly string[] | undefined): string[] {
  const seen = new Set<string>();
  for (const value of values ?? []) {
    const text = typeof value === "string" ? value.trim() : "";
    if (!text || text.length > 300) continue;
    seen.add(text);
    if (seen.size >= MAX_MENU_VALUES) break;
  }
  return [...seen];
}

export function taskMenuItems(options: {
  cards: readonly TaskCard[];
  column?: TaskStatus | null;
  busy?: boolean;
  vocabulary?: TaskMenuVocabulary;
  /** False while no repository is registered, so a handoff cannot be offered. */
  canHandoff?: boolean;
}): TaskMenuItem[] {
  const cards = options.cards.filter((card) => isTaskId(card.id) && isRevision(card.revision));
  const busy = options.busy === true;
  const many = cards.length > 1;
  const single = cards.length === 1 ? cards[0] : null;
  if (cards.length === 0) {
    const status = options.column;
    if (!status) return [];
    return [{
      id: `new-${status}`,
      label: `New task in ${STATUS_LABELS[status]}`,
      action: { kind: "newInColumn", status },
      icon: "add",
      disabled: busy,
    }];
  }

  const items: TaskMenuItem[] = [];
  if (single) {
    items.push({ id: "open", label: "Open", action: { kind: "open" }, icon: "open", disabled: busy, hint: "O" });
    items.push({
      id: "enhance",
      label: "Quick Enhance…",
      action: { kind: "enhance" },
      icon: "enhance",
      disabled: busy,
      hint: "E",
    });
    if (options.canHandoff !== false) {
      items.push({
        id: "agent-menu",
        label: "Send to agent…",
        action: { kind: "submenu", submenu: "agent" },
        icon: "agent",
        disabled: busy,
      });
      for (const entry of AGENT_TARGETS) {
        items.push({
          id: entry.id,
          label: entry.label,
          action: { kind: "agent", target: entry.target },
          icon: "agent",
          hint: entry.hint,
          group: "agent",
          disabled: busy,
        });
      }
    }
    items.push({
      id: "duplicate",
      label: "Duplicate…",
      action: { kind: "duplicate" },
      icon: "duplicate",
      disabled: busy,
    });
  }

  items.push({
    id: "copy-menu",
    label: "Copy…",
    action: { kind: "submenu", submenu: "copy" },
    icon: "copy",
    separatorBefore: items.length > 0,
    disabled: busy,
  });
  items.push({
    id: "copy-agent",
    label: many ? `${cards.length} tasks for agent` : "For an AI agent",
    action: { kind: "copyAgent" },
    icon: "brief",
    group: "copy",
    hint: "⇧⌘C",
    disabled: busy,
  });
  items.push({
    id: "copy-title",
    label: many ? "Titles" : "Title",
    action: { kind: "copyTitle" },
    icon: "copy",
    group: "copy",
    disabled: busy,
  });
  if (single) {
    items.push({ id: "copy-id", label: "Task ID", action: { kind: "copyId" }, icon: "id", group: "copy", disabled: busy });
    items.push({ id: "copy-brief", label: "Saved brief", action: { kind: "copyBrief" }, icon: "brief", group: "copy", disabled: busy });
  }

  items.push({
    id: "move-menu",
    label: "Move to…",
    action: { kind: "submenu", submenu: "move" },
    icon: "move",
    separatorBefore: true,
    disabled: busy,
  });
  for (const status of STATUSES) {
    const checked = shared(cards, (card) => card.status, status);
    items.push({
      id: `move-${status}`,
      label: STATUS_LABELS[status],
      action: { kind: "move", status },
      group: "move",
      checked,
      disabled: busy || checked === true,
    });
  }

  items.push({
    id: "priority-menu",
    label: "Set priority…",
    action: { kind: "submenu", submenu: "priority" },
    icon: "priority",
    disabled: busy,
  });
  for (const priority of [0, 1, 2, 3] as const) {
    const checked = shared(cards, (card) => card.priority, priority as number);
    items.push({
      id: `priority-${priority}`,
      label: PRIORITY_LABELS[priority],
      action: { kind: "priority", priority },
      group: "priority",
      checked,
      disabled: busy || checked === true,
    });
  }

  items.push({
    id: "due-menu",
    label: "Due…",
    action: { kind: "submenu", submenu: "due" },
    icon: "due",
    disabled: busy,
  });
  for (const entry of DUE_CHOICES) {
    // Only "No due date" can be known to be the current value: the other
    // choices are relative to now, so a task due Friday is not "Next week"
    // in any sense this menu could check.
    const current = entry.choice === "clear" && shared(cards, (card) => card.due_at ?? null, null);
    items.push({
      id: `due-${entry.choice}`,
      label: entry.label,
      action: { kind: "due", choice: entry.choice },
      group: "due",
      checked: current,
      // A set-style row showing the current value is disabled, exactly as the
      // status and priority rows are: choosing it would spend a revision to
      // write what is already there.
      disabled: busy || current === true,
    });
  }

  const owners = bounded(options.vocabulary?.owners);
  items.push({
    id: "owner-menu",
    label: "Owner…",
    action: { kind: "submenu", submenu: "owner" },
    icon: "owner",
    disabled: busy,
  });
  const unassigned = shared(cards, (card) => (card.owner ?? "").trim(), "");
  items.push({
    id: "owner-none",
    label: "Unassigned",
    action: { kind: "owner", owner: null },
    group: "owner",
    checked: unassigned,
    disabled: busy || unassigned === true,
  });
  for (const owner of owners) {
    const current = shared(cards, (card) => (card.owner ?? "").trim(), owner);
    items.push({
      id: `owner-${owner}`,
      label: owner,
      action: { kind: "owner", owner },
      group: "owner",
      checked: current,
      disabled: busy || current === true,
    });
  }

  const labels = bounded(options.vocabulary?.labels);
  if (labels.length) {
    items.push({
      id: "label-menu",
      label: "Labels…",
      action: { kind: "submenu", submenu: "label" },
      icon: "label",
      disabled: busy,
    });
    for (const label of labels) {
      const state = labelState(cards, label);
      items.push({
        id: `label-${label}`,
        label,
        action: { kind: "label", label, add: state !== true },
        group: "label",
        checked: state,
        disabled: busy,
      });
    }
  }

  if (options.column) {
    items.push({
      id: "select-column",
      label: `Select all in ${STATUS_LABELS[options.column]}`,
      action: { kind: "selectColumn" },
      icon: "select",
      separatorBefore: true,
      disabled: busy,
    });
    items.push({
      id: `new-${options.column}`,
      label: `New task in ${STATUS_LABELS[options.column]}`,
      action: { kind: "newInColumn", status: options.column },
      icon: "add",
      hint: "N",
      disabled: busy,
    });
  }

  // Archiving and deleting are the two ways work leaves the board, so they
  // are one group. The hint names Done on every row, enabled or not: it is
  // the only place a reader is told where an archived task actually goes.
  const archived = archiveState(cards);
  items.push({
    id: "archive",
    label: many ? `Archive ${cards.length} tasks` : "Archive",
    action: { kind: "archive" },
    icon: "archive",
    separatorBefore: true,
    hint: archived === "all"
      ? `Already in ${STATUS_LABELS[ARCHIVE_STATUS]}`
      : STATUS_LABELS[ARCHIVE_STATUS],
    // Every task in the selection is already archived, so every write would
    // spend a revision storing the value that is already there — the same
    // reason a Move row showing the current status is disabled rather than
    // hidden. A partly archived selection is offered: the rest is a change.
    disabled: busy || archived === "all",
  });
  items.push({
    id: "delete",
    label: many ? `Delete ${cards.length} tasks…` : "Delete task…",
    action: { kind: "delete" },
    icon: "delete",
    danger: true,
    hint: "⌫",
    disabled: busy,
  });
  return items;
}
