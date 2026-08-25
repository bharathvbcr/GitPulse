import type { BranchFolder, BranchInfo, BranchSection, TagInfo } from "./types";

const STALE_SECONDS = 90 * 24 * 60 * 60;

export function fuzzyMatch(query: string, text: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  const t = text.toLowerCase();
  if (t.includes(q)) return true;
  let qi = 0;
  for (let i = 0; i < t.length && qi < q.length; i++) {
    if (t[i] === q[qi]) qi += 1;
  }
  return qi === q.length;
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

function ensureFolder(folders: BranchFolder[], idPrefix: string, parts: string[]): BranchFolder {
  let list = folders;
  let folder: BranchFolder | undefined;
  let path = idPrefix;
  for (const part of parts) {
    path = `${path}/${part}`;
    let found = list.find((f) => f.label === part);
    if (!found) {
      found = { id: path, label: part, folders: [], branches: [] };
      list.push(found);
    }
    folder = found;
    list = found.folders;
  }
  if (!folder) {
    folder = { id: path, label: parts[0] || "folder", folders: [], branches: [] };
    folders.push(folder);
  }
  return folder;
}

function insertBranch(section: BranchSection, displayName: string, branch: BranchInfo) {
  const { folders } = splitPath(displayName);
  if (folders.length === 0) {
    section.branches.push(branch);
    return;
  }
  const folder = ensureFolder(section.folders, section.id, folders);
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

export function groupBranches(branches: BranchInfo[], tags: TagInfo[] = []): BranchSection[] {
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

  for (const branch of branches) {
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
      const prefix = `${remote}/`;
      const display = branch.name.startsWith(prefix) ? branch.name.slice(prefix.length) : branch.name;
      insertBranch(section, display, branch);
    } else {
      insertBranch(local, branch.name, branch);
    }
  }

  sortSection(local);
  const remoteSections = [...remotes.values()].sort((a, b) => a.label.localeCompare(b.label));
  remoteSections.forEach(sortSection);

  const sections: BranchSection[] = [];
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

function branchMatches(branch: BranchInfo, query: string): boolean {
  return (
    fuzzyMatch(query, branch.name) ||
    fuzzyMatch(query, branch.last_summary || "") ||
    fuzzyMatch(query, branch.last_author || "")
  );
}

function filterFolder(folder: BranchFolder, query: string): BranchFolder | null {
  const q = query.toLowerCase();
  if (folder.label.toLowerCase().includes(q)) {
    return folder;
  }
  const folders = folder.folders
    .map((child) => filterFolder(child, query))
    .filter((child): child is BranchFolder => child !== null);
  const branches = folder.branches.filter((branch) => branchMatches(branch, query));
  if (folders.length === 0 && branches.length === 0) return null;
  return { ...folder, folders, branches };
}

export function filterBranchSections(sections: BranchSection[], query: string): BranchSection[] {
  const q = query.trim();
  if (!q) return sections;
  return sections
    .map((section) => {
      if (section.kind === "tags") {
        const tags = section.tags.filter(
          (tag) => fuzzyMatch(q, tag.name) || (tag.message ? fuzzyMatch(q, tag.message) : false)
        );
        if (tags.length === 0 && !fuzzyMatch(q, section.label)) return null;
        return { ...section, tags, branchCount: tags.length };
      }
      if (fuzzyMatch(q, section.label) && q.length === section.label.length) {
        return section;
      }
      const folders = section.folders
        .map((folder) => filterFolder(folder, q))
        .filter((folder): folder is BranchFolder => folder !== null);
      const branches = section.branches.filter((branch) => branchMatches(branch, q));
      if (folders.length === 0 && branches.length === 0) return null;
      const branchCount = branches.length + folders.reduce((n, folder) => n + countFolder(folder), 0);
      return { ...section, folders, branches, branchCount };
    })
    .filter((section): section is BranchSection => section !== null);
}
