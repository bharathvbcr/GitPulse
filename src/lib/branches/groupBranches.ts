import type { BranchFolder, BranchInfo, BranchSection, TagInfo, BranchFilterTab } from "./types";

const STALE_SECONDS = 90 * 24 * 60 * 60;

export interface MatchChunk {
  text: string;
  matched: boolean;
}

/** Case-folds `text` against an already-lowercased query — hot-path helper. */
export function matchFolded(q: string, foldedText: string): boolean {
  if (!q) return true;
  if (foldedText.includes(q)) return true;
  let qi = 0;
  for (let i = 0; i < foldedText.length && qi < q.length; i++) {
    if (foldedText[i] === q[qi]) qi += 1;
  }
  return qi === q.length;
}

export function fuzzyMatch(query: string, text: string): boolean {
  return matchFolded(query.trim().toLowerCase(), text.toLowerCase());
}

/** Computes slice chunks for UI search highlight with zero regex. */
export function highlightMatches(text: string, query: string): MatchChunk[] {
  const q = query.trim().toLowerCase();
  if (!q || !text) return [{ text, matched: false }];
  const lower = text.toLowerCase();

  // Substring match takes precedence
  const subIdx = lower.indexOf(q);
  if (subIdx !== -1) {
    const chunks: MatchChunk[] = [];
    if (subIdx > 0) chunks.push({ text: text.slice(0, subIdx), matched: false });
    chunks.push({ text: text.slice(subIdx, subIdx + q.length), matched: true });
    if (subIdx + q.length < text.length) {
      chunks.push({ text: text.slice(subIdx + q.length), matched: false });
    }
    return chunks;
  }

  // Subsequence match fallback
  const chunks: MatchChunk[] = [];
  let qi = 0;
  let currMatched = false;
  let currBuf = "";

  for (let i = 0; i < text.length; i++) {
    const isCharMatch = qi < q.length && lower[i] === q[qi];
    if (isCharMatch) qi++;

    if (isCharMatch === currMatched) {
      currBuf += text[i];
    } else {
      if (currBuf) chunks.push({ text: currBuf, matched: currMatched });
      currBuf = text[i];
      currMatched = isCharMatch;
    }
  }
  if (currBuf) chunks.push({ text: currBuf, matched: currMatched });
  return qi === q.length ? chunks : [{ text, matched: false }];
}

export function localNameFor(branch: BranchInfo): string {
  if (!branch.is_remote) return branch.name;
  if (branch.remote_name && branch.name.startsWith(`${branch.remote_name}/`)) {
    return branch.name.slice(branch.remote_name.length + 1);
  }
  return branch.name;
}

export function branchLeafName(branch: BranchInfo): string {
  const name = localNameFor(branch);
  const parts = name.split("/").filter(Boolean);
  return parts[parts.length - 1] || branch.name;
}

export function isStaleBranch(timestamp: number, nowSec: number = Date.now() / 1000): boolean {
  if (!timestamp) return false;
  return nowSec - timestamp > STALE_SECONDS;
}

function compareBranches(a: BranchInfo, b: BranchInfo): number {
  if (a.is_current !== b.is_current) return a.is_current ? -1 : 1;
  if (a.is_default !== b.is_default) return a.is_default ? -1 : 1;
  return a.name.localeCompare(b.name);
}

function splitPath(name: string): { folders: string[]; leaf: string } {
  const parts = name.split("/").filter(Boolean);
  if (parts.length <= 1) return { folders: [], leaf: name };
  return { folders: parts.slice(0, -1), leaf: parts[parts.length - 1] };
}

/**
 * Path-indexed folder walk: one Map hit per path part instead of a linear
 * sibling scan, so grouping is O(total parts) across a whole section.
 * Callers pass the per-section index and the section's root folder list;
 * `parts` is never empty (insertBranch guards).
 */
function ensureFolder(
  index: Map<string, BranchFolder>,
  roots: BranchFolder[],
  idPrefix: string,
  parts: string[]
): BranchFolder {
  let list = roots;
  let folder: BranchFolder | undefined;
  let path = idPrefix;
  for (const part of parts) {
    path = `${path}/${part}`;
    folder = index.get(path);
    if (!folder) {
      folder = { id: path, label: part, folders: [], branches: [] };
      list.push(folder);
      index.set(path, folder);
    }
    list = folder.folders;
  }
  return folder!;
}

function insertBranch(
  index: Map<string, BranchFolder>,
  section: BranchSection,
  displayName: string,
  branch: BranchInfo
) {
  const { folders } = splitPath(displayName);
  if (folders.length === 0) {
    section.branches.push(branch);
    return;
  }
  const folder = ensureFolder(index, section.folders, section.id, folders);
  folder.branches.push(branch);
}

function sortFolder(folder: BranchFolder) {
  folder.folders.sort((a, b) => a.label.localeCompare(b.label));
  folder.branches.sort(compareBranches);
  folder.folders.forEach(sortFolder);
}

export function countFolder(folder: BranchFolder): number {
  return folder.branches.length + folder.folders.reduce((n, child) => n + countFolder(child), 0);
}

function sortSection(section: BranchSection) {
  section.folders.sort((a, b) => a.label.localeCompare(b.label));
  section.branches.sort(compareBranches);
  section.folders.forEach(sortFolder);
  section.branchCount =
    section.kind === "tags"
      ? section.tags.length
      : section.branches.length + section.folders.reduce((n, folder) => n + countFolder(folder), 0);
}

export function groupBranches(
  branches: BranchInfo[],
  tags: TagInfo[] = [],
  pinnedBranchNames: Set<string> = new Set()
): BranchSection[] {
  const local: BranchSection = {
    id: "local",
    label: "Local",
    kind: "local",
    folders: [],
    branches: [],
    tags: [],
    branchCount: 0,
  };
  const remotes = new Map<string, BranchSection>();
  const indexes = new Map<string, Map<string, BranchFolder>>();
  const indexFor = (sectionId: string): Map<string, BranchFolder> => {
    let index = indexes.get(sectionId);
    if (!index) {
      index = new Map();
      indexes.set(sectionId, index);
    }
    return index;
  };

  const pinnedBranches: BranchInfo[] = [];

  for (const branch of branches) {
    if (pinnedBranchNames.has(branch.name)) {
      pinnedBranches.push(branch);
    }
    if (branch.is_remote) {
      const remote = branch.remote_name || "origin";
      let section = remotes.get(remote);
      if (!section) {
        section = {
          id: `remote:${remote}`,
          label: remote,
          kind: "remote",
          remoteName: remote,
          folders: [],
          branches: [],
          tags: [],
          branchCount: 0,
        };
        remotes.set(remote, section);
      }
      const display =
        branch.name.length > remote.length &&
        branch.name.startsWith(remote) &&
        branch.name.charCodeAt(remote.length) === 47
          ? branch.name.slice(remote.length + 1)
          : branch.name;
      insertBranch(indexFor(section.id), section, display, branch);
    } else {
      insertBranch(indexFor(local.id), local, branch.name, branch);
    }
  }

  sortSection(local);
  const remoteSections = [...remotes.values()].sort((a, b) => a.label.localeCompare(b.label));
  remoteSections.forEach(sortSection);

  const sections: BranchSection[] = [];

  if (pinnedBranches.length > 0) {
    pinnedBranches.sort(compareBranches);
    sections.push({
      id: "pinned",
      label: "Pinned",
      kind: "pinned",
      folders: [],
      branches: pinnedBranches,
      tags: [],
      branchCount: pinnedBranches.length,
    });
  }

  if (local.branchCount > 0) sections.push(local);
  sections.push(...remoteSections);

  if (tags.length > 0) {
    const sorted = [...tags].sort((a, b) => b.name.localeCompare(a.name));
    sections.push({
      id: "tags",
      label: "Tags",
      kind: "tags",
      folders: [],
      branches: [],
      tags: sorted,
      branchCount: sorted.length,
    });
  }
  return sections;
}

// Stable text cache keyed on branch name + tip_commit_id so object mutations do NOT evict string cache
const textCache = new Map<string, { name: string; summary: string; author: string }>();

function getFoldedBranch(branch: BranchInfo) {
  const key = `${branch.name}@${branch.tip_commit_id}`;
  let cached = textCache.get(key);
  if (!cached) {
    cached = {
      name: branch.name.toLowerCase(),
      summary: (branch.last_summary || "").toLowerCase(),
      author: (branch.last_author || "").toLowerCase(),
    };
    textCache.set(key, cached);
    if (textCache.size > 25000) textCache.clear();
  }
  return cached;
}

function branchMatches(branch: BranchInfo, q: string, tab: BranchFilterTab, nowSec: number): boolean {
  if (tab === "local" && branch.is_remote) return false;
  if (tab === "remote" && !branch.is_remote) return false;
  if (tab === "active" && isStaleBranch(branch.last_commit_timestamp, nowSec)) return false;
  if (tab === "stale" && !isStaleBranch(branch.last_commit_timestamp, nowSec)) return false;

  if (!q) return true;
  const folded = getFoldedBranch(branch);
  return (
    matchFolded(q, folded.name) ||
    matchFolded(q, folded.summary) ||
    matchFolded(q, folded.author)
  );
}

function filterFolder(folder: BranchFolder, q: string, tab: BranchFilterTab, nowSec: number): BranchFolder | null {
  if (q && folder.label.toLowerCase().includes(q)) {
    if (tab === "all") return folder;
    const folders = folder.folders
      .map((child) => filterFolder(child, "", tab, nowSec))
      .filter((child): child is BranchFolder => child !== null);
    const branches = folder.branches.filter((branch) => branchMatches(branch, "", tab, nowSec));
    if (folders.length === 0 && branches.length === 0) return null;
    return { ...folder, folders, branches };
  }
  const folders = folder.folders
    .map((child) => filterFolder(child, q, tab, nowSec))
    .filter((child): child is BranchFolder => child !== null);
  const branches = folder.branches.filter((branch) => branchMatches(branch, q, tab, nowSec));
  if (folders.length === 0 && branches.length === 0) return null;
  return { ...folder, folders, branches };
}

export function filterBranchSections(
  sections: BranchSection[],
  query: string,
  tab: BranchFilterTab = "all",
  nowSec: number = Date.now() / 1000
): BranchSection[] {
  const q = query.trim().toLowerCase();
  if (!q && tab === "all") return sections;

  return sections
    .map((section) => {
      if (tab === "local" && section.kind !== "local" && section.kind !== "pinned") return null;
      if (tab === "remote" && section.kind !== "remote") return null;
      if (tab === "tags" && section.kind !== "tags") return null;
      if (tab === "pinned" && section.kind !== "pinned") return null;

      if (section.kind === "tags") {
        if (tab === "local" || tab === "remote" || tab === "active" || tab === "stale" || tab === "pinned") return null;
        const tags = section.tags.filter(
          (tag) => fuzzyMatch(q, tag.name) || (tag.message ? fuzzyMatch(q, tag.message) : false)
        );
        if (tags.length === 0 && (q && !fuzzyMatch(q, section.label))) return null;
        return { ...section, tags, branchCount: tags.length };
      }

      if (q && fuzzyMatch(q, section.label) && q.length === section.label.length) {
        return section;
      }

      const folders = section.folders
        .map((folder) => filterFolder(folder, q, tab, nowSec))
        .filter((folder): folder is BranchFolder => folder !== null);
      const branches = section.branches.filter((branch) => branchMatches(branch, q, tab, nowSec));
      if (folders.length === 0 && branches.length === 0) return null;
      const branchCount = branches.length + folders.reduce((n, folder) => n + countFolder(folder), 0);
      return { ...section, folders, branches, branchCount };
    })
    .filter((section): section is BranchSection => section !== null);
}
