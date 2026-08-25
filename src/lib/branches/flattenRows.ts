import type { BranchFolder, BranchInfo, BranchSection, TagInfo } from "./types";

export interface SectionHeaderRow {
  kind: "section-header";
  depth: 0;
  /** Section object carried alongside its id so renderers skip a lookup pass. */
  section: BranchSection;
  sectionId: string;
  key: string;
}

export interface FolderHeaderRow {
  kind: "folder-header";
  depth: number;
  sectionId: string;
  folderId: string;
  folder: BranchFolder;
  key: string;
}

export interface BranchRow {
  kind: "branch";
  depth: number;
  branch: BranchInfo;
  key: string;
}

export interface TagRow {
  kind: "tag";
  depth: 0;
  tag: TagInfo;
  key: string;
}

export type FlatRow = SectionHeaderRow | FolderHeaderRow | BranchRow | TagRow;

/** Mirrors BranchList's collapsed-state lookup (folders pass kind "local"). */
export type CollapsedLookup = (id: string, kind: BranchSection["kind"]) => boolean;

/**
 * Flattens grouped sections into render-order rows for the sidebar's single
 * shared scroller. Collapsed sections/folders contribute only their header
 * row. Keys are deterministic section/folder/name prefixes; uniqueness within
 * a section is guaranteed upstream by git ref naming, not re-enforced here.
 */
export function flattenRows(sections: BranchSection[], isCollapsed: CollapsedLookup): FlatRow[] {
  const rows: FlatRow[] = [];

  const pushFolders = (folders: BranchFolder[], depth: number, sectionId: string): void => {
    for (const folder of folders) {
      rows.push({
        kind: "folder-header",
        depth,
        sectionId,
        folderId: folder.id,
        folder,
        key: `f:${folder.id}`,
      });
      if (isCollapsed(folder.id, "local")) continue;
      pushFolders(folder.folders, depth + 1, sectionId);
      for (const branch of folder.branches) {
        rows.push({
          kind: "branch",
          depth: depth + 1,
          branch,
          key: `b:${sectionId}:${branch.name}`,
        });
      }
    }
  };

  for (const section of sections) {
    rows.push({
      kind: "section-header",
      depth: 0,
      section,
      sectionId: section.id,
      key: `s:${section.id}`,
    });
    if (isCollapsed(section.id, section.kind)) continue;
    if (section.kind === "tags") {
      for (const tag of section.tags) {
        rows.push({ kind: "tag", depth: 0, tag, key: `t:${tag.name}` });
      }
      continue;
    }
    pushFolders(section.folders, 0, section.id);
    for (const branch of section.branches) {
      rows.push({ kind: "branch", depth: 0, branch, key: `b:${section.id}:${branch.name}` });
    }
  }
  return rows;
}
