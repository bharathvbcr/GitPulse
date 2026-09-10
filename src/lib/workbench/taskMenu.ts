import { PRIORITY_LABELS } from "./boardDrag";
import { STATUSES, STATUS_LABELS, type TaskCard, type TaskStatus } from "./client";
import { displayTitle, isRevision, isTaskId } from "./taskDelete";

export type TaskMenuSubmenu = "move" | "priority" | "copy";
export type TaskMenuIcon =
  | "open"
  | "enhance"
  | "duplicate"
  | "copy"
  | "id"
  | "brief"
  | "move"
  | "priority"
  | "select"
  | "add"
  | "delete";

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
  | { kind: "selectColumn" }
  | { kind: "newInColumn"; status: TaskStatus }
  | { kind: "delete" }
  | { kind: "submenu"; submenu: TaskMenuSubmenu };

export interface TaskMenuItem {
  id: string;
  label: string;
  action: TaskMenuAction;
  disabled?: boolean;
  danger?: boolean;
  hint?: string;
  separatorBefore?: boolean;
  icon?: TaskMenuIcon;
  group?: TaskMenuSubmenu;
}

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
  if (page === "copy") return "Copy";
  if (page === "move") return "Move to";
  if (page === "priority") return "Priority";
  return "Task actions";
}

export function taskMenuItems(options: {
  cards: readonly TaskCard[];
  column?: TaskStatus | null;
  busy?: boolean;
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
    items.push({ id: "open", label: "Open", action: { kind: "open" }, icon: "open", disabled: busy });
    items.push({
      id: "enhance",
      label: "Quick Enhance…",
      action: { kind: "enhance" },
      icon: "enhance",
      disabled: busy,
      hint: "Manvi",
    });
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

  const sharedStatus = cards.every((card) => card.status === cards[0].status) ? cards[0].status : null;
  const moveTargets = STATUSES.filter((status) => status !== sharedStatus);
  if (moveTargets.length) {
    items.push({
      id: "move-menu",
      label: "Move to…",
      action: { kind: "submenu", submenu: "move" },
      icon: "move",
      separatorBefore: true,
      disabled: busy,
    });
    for (const status of moveTargets) {
      items.push({
        id: `move-${status}`,
        label: STATUS_LABELS[status],
        action: { kind: "move", status },
        group: "move",
        disabled: busy,
      });
    }
  }

  const sharedPriority = cards.every((card) => card.priority === cards[0].priority) ? cards[0].priority : null;
  const priorityTargets = ([0, 1, 2, 3] as const).filter((priority) => priority !== sharedPriority);
  if (priorityTargets.length) {
    items.push({
      id: "priority-menu",
      label: "Set priority…",
      action: { kind: "submenu", submenu: "priority" },
      icon: "priority",
      disabled: busy,
    });
    for (const priority of priorityTargets) {
      items.push({
        id: `priority-${priority}`,
        label: PRIORITY_LABELS[priority],
        action: { kind: "priority", priority },
        group: "priority",
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
      disabled: busy,
    });
  }

  items.push({
    id: "delete",
    label: many ? `Delete ${cards.length} tasks…` : "Delete task…",
    action: { kind: "delete" },
    icon: "delete",
    danger: true,
    separatorBefore: true,
    disabled: busy,
  });
  return items;
}
