/**
 * Open editor files survive quitting the app.
 *
 * Draft text does not: conflict drafts have their own store, and every other
 * unsaved buffer is discarded on purpose. What comes back is which files were
 * open, which one was the preview tab, and which one was active.
 *
 * A save that shrinks the list only sticks when the caller passes `commit`.
 * Preview and activation writes do not, so a partial snapshot cannot close
 * files the user still had open.
 */

import { formatPathParts } from "./formatPath";
import { browserStorage, type StorageLike } from "../repos/persist";
import type { EditorTabState } from "./editorTabs";

export const EDITOR_TAB_STORAGE_KEY = "gitpulse_editor_tabs_v1";
export const MAX_EDITOR_TABS = 40;
export const MAX_EDITOR_REPOS = 24;

const MAX_PATH = 1024;
const CONTROL = /[\u0000-\u001F\u007F]/;

export interface PersistedEditorTab {
  path: string;
  preview: boolean;
}

interface PersistedEditorRepo {
  epoch: number;
  tabs: PersistedEditorTab[];
  active: string | null;
}

interface EditorBlob {
  version: 1;
  repos: Record<string, PersistedEditorRepo>;
}

function emptyBlob(): EditorBlob {
  return { version: 1, repos: {} };
}

function cleanPath(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const path = value.replace(/\\/g, "/").trim();
  if (!path || path.length > MAX_PATH || CONTROL.test(path)) return null;
  if (path.startsWith("/") || path.split("/").includes("..")) return null;
  return path;
}

/** Repository keys are absolute paths; file paths inside a repo are not. */
function repoKey(value: string): string | null {
  if (!value || value.length > MAX_PATH || CONTROL.test(value)) return null;
  return value.replace(/\\/g, "/");
}

function readBlob(storage: StorageLike): EditorBlob {
  try {
    const parsed: unknown = JSON.parse(storage.getItem(EDITOR_TAB_STORAGE_KEY) ?? "");
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return emptyBlob();
    const repos = (parsed as { repos?: unknown }).repos;
    if (!repos || typeof repos !== "object" || Array.isArray(repos)) return emptyBlob();
    const out = emptyBlob();
    for (const [repo, raw] of Object.entries(repos as Record<string, unknown>)) {
      const repoPath = repoKey(repo);
      if (!repoPath || !raw || typeof raw !== "object" || Array.isArray(raw)) continue;
      const record = raw as { epoch?: unknown; tabs?: unknown; active?: unknown };
      const tabs: PersistedEditorTab[] = [];
      const seen = new Set<string>();
      if (Array.isArray(record.tabs)) {
        for (const item of record.tabs) {
          if (!item || typeof item !== "object") continue;
          const path = cleanPath((item as { path?: unknown }).path);
          if (!path || seen.has(path)) continue;
          seen.add(path);
          tabs.push({ path, preview: (item as { preview?: unknown }).preview === true });
          if (tabs.length >= MAX_EDITOR_TABS) break;
        }
      }
      const active = cleanPath(record.active);
      const epoch = typeof record.epoch === "number" && Number.isInteger(record.epoch) && record.epoch >= 0
        ? record.epoch
        : 0;
      out.repos[repoPath] = {
        epoch,
        tabs,
        active: active && tabs.some((tab) => tab.path === active) ? active : tabs[0]?.path ?? null,
      };
    }
    return out;
  } catch {
    return emptyBlob();
  }
}

function toState(record: PersistedEditorRepo): EditorTabState {
  let previewSeen = false;
  const tabs = record.tabs.map((tab) => {
    const preview = tab.preview && !previewSeen;
    if (preview) previewSeen = true;
    return { path: tab.path, name: formatPathParts(tab.path).name, preview };
  });
  return { tabs, active: record.active, drafts: {} };
}

export function loadPersistedEditorTabs(
  repo: string,
  storage: StorageLike | null = browserStorage(),
): EditorTabState | null {
  const key = repoKey(repo);
  if (!storage || !key) return null;
  const record = readBlob(storage).repos[key];
  if (!record || record.tabs.length === 0) return null;
  return toState(record);
}

/**
 * @param commit when true, a shorter list replaces the saved one. Otherwise
 * a shorter list is ignored and the saved files stay open.
 */
export function savePersistedEditorTabs(
  repo: string,
  state: { tabs: readonly { path: string; preview: boolean }[]; active: string | null },
  commit: boolean,
  storage: StorageLike | null = browserStorage(),
): boolean {
  const key = repoKey(repo);
  if (!storage || !key) return false;
  const blob = readBlob(storage);
  const previous = blob.repos[key];
  const nextTabs: PersistedEditorTab[] = [];
  const seen = new Set<string>();
  for (const tab of state.tabs) {
    const path = cleanPath(tab.path);
    if (!path || seen.has(path)) continue;
    seen.add(path);
    nextTabs.push({ path, preview: tab.preview === true });
    if (nextTabs.length >= MAX_EDITOR_TABS) break;
  }
  const previousPaths = new Set(previous?.tabs.map((tab) => tab.path) ?? []);
  const lost = [...previousPaths].some((path) => !seen.has(path));
  if (lost && !commit) return false;
  const active = cleanPath(state.active);
  delete blob.repos[key];
  blob.repos[key] = {
    epoch: (previous?.epoch ?? 0) + (lost ? 1 : 0),
    tabs: nextTabs,
    active: active && seen.has(active) ? active : nextTabs[0]?.path ?? null,
  };
  const repos = Object.keys(blob.repos);
  if (repos.length > MAX_EDITOR_REPOS) {
    for (const extra of repos.slice(0, repos.length - MAX_EDITOR_REPOS)) {
      if (extra !== key) delete blob.repos[extra];
    }
  }
  try {
    storage.setItem(EDITOR_TAB_STORAGE_KEY, JSON.stringify(blob));
    return true;
  } catch {
    return false;
  }
}
