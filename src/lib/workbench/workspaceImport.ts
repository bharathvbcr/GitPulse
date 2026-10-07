/**
 * Turning the repository tab strip's named groups into durable workspaces.
 *
 * Tab groups live only in this window's saved layout (`repos/persist.ts`):
 * close the tabs and the grouping is gone, and the task board cannot see it.
 * A workspace is the durable form — membership that survives closing tabs,
 * restarts and group changes. Importing makes one workspace per named group
 * and registers each tab's repository into it.
 *
 * Idempotent by name. A group whose name a workspace already has is skipped
 * and said so, never merged into: that workspace may hold repositories on
 * purpose that the tab group does not, and an import is not the place to
 * decide. Every outcome — created, skipped, a repository that could not be
 * registered — is reported, so a partial import never reads as a full one.
 */
import { explainError, newID, putWorkspace, registerRepository, type Repository, type Workspace, type WorkspaceCard } from "./client";
import { WORKSPACE_SPACING } from "./workspaceOrder";

export interface TabGroupSource { path: string; group?: string | null }
export interface GroupColorSource { group: string; color: string }

export interface TabGroupPlan { name: string; color: string; paths: string[] }

/** The same name, the way a reader would judge it: trimmed, case-folded. */
export function workspaceNameKey(name: string): string {
  return name.trim().toLocaleLowerCase();
}

/** Named groups on the strip, in the order their first tab appears. */
export function tabGroups(tabs: readonly TabGroupSource[], colors: readonly GroupColorSource[] = []): TabGroupPlan[] {
  const groups = new Map<string, TabGroupPlan>();
  for (const tab of tabs) {
    const name = tab.group?.trim();
    if (!name) continue;
    const key = workspaceNameKey(name);
    const existing = groups.get(key);
    if (existing) { if (!existing.paths.includes(tab.path)) existing.paths.push(tab.path); continue; }
    const color = colors.find((entry) => workspaceNameKey(entry.group) === key)?.color ?? "";
    groups.set(key, { name, color, paths: [tab.path] });
  }
  return [...groups.values()];
}

export interface ImportIO {
  register: (path: string) => Promise<Repository>;
  put: (input: Record<string, unknown>) => Promise<Workspace>;
}
const defaultIO: ImportIO = { register: registerRepository, put: putWorkspace };

export interface ImportReport {
  created: { name: string; repositories: number }[];
  skipped: { name: string; reason: string }[];
  failed: { name: string; error: string }[];
}

export function importSummary(report: ImportReport): string {
  const parts: string[] = [];
  if (report.created.length) parts.push(`Created ${report.created.length} ${report.created.length === 1 ? "workspace" : "workspaces"}`);
  if (report.skipped.length) parts.push(`skipped ${report.skipped.length} ${report.skipped.length === 1 ? "group that already has" : "groups that already have"} a workspace`);
  if (report.failed.length) parts.push(`${report.failed.length} could not be imported`);
  const sentence = parts.join(", ");
  return sentence ? `${sentence.charAt(0).toUpperCase()}${sentence.slice(1)}.` : "No tab groups to import.";
}

/**
 * Import every named group that has no workspace yet.
 *
 * New workspaces go after the existing ones, in strip order, so an import
 * never reorders what the reader already arranged. Repositories are
 * registered through the same host path as Add repository, so two worktrees
 * of one repository become one member, not two.
 */
export async function importTabGroups(
  plans: readonly TabGroupPlan[],
  existing: readonly Pick<WorkspaceCard, "name" | "position">[],
  io: ImportIO = defaultIO,
): Promise<ImportReport> {
  const report: ImportReport = { created: [], skipped: [], failed: [] };
  const taken = new Set(existing.map((group) => workspaceNameKey(group.name)));
  let position = existing.reduce((max, group) => Math.max(max, group.position), 0);
  for (const plan of plans) {
    if (taken.has(workspaceNameKey(plan.name))) {
      report.skipped.push({ name: plan.name, reason: "A workspace with this name already exists." });
      continue;
    }
    const ids: string[] = [];
    const unregistered: string[] = [];
    for (const path of plan.paths) {
      try {
        const repo = await io.register(path);
        if (!ids.includes(repo.id)) ids.push(repo.id);
      } catch (cause) {
        unregistered.push(`${path}: ${explainError(cause)}`);
      }
    }
    if (!ids.length) {
      report.failed.push({ name: plan.name, error: `No repository in this group could be registered. ${unregistered.join("; ")}` });
      continue;
    }
    position += WORKSPACE_SPACING;
    try {
      const id = newID();
      await io.put({
        id, request_id: newID(), expected_revision: 0,
        name: plan.name, description: "", icon: "", color: plan.color,
        position, pinned: false, archived: false, repository_ids: ids,
      });
      taken.add(workspaceNameKey(plan.name));
      report.created.push({ name: plan.name, repositories: ids.length });
      if (unregistered.length) report.failed.push({ name: plan.name, error: `Created without ${unregistered.length === 1 ? "one repository" : `${unregistered.length} repositories`}: ${unregistered.join("; ")}` });
    } catch (cause) {
      report.failed.push({ name: plan.name, error: explainError(cause) });
    }
  }
  return report;
}
