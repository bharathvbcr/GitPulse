import { describe, expect, it } from "vitest";
import {
  FILE_DASHBOARD_WIDTH,
  FILE_EDITOR_MIN_WIDTH,
  FILE_EXPLORER_WIDTH,
  resolveFilePaneLayout,
} from "./filePaneLayout";

describe("resolveFilePaneLayout", () => {
  it("keeps a usable editor at 900px instead of mounting both fixed rails", () => {
    expect(resolveFilePaneLayout({
      containerWidth: 900,
      explorerRequested: true,
      dashboardRequested: true,
      preferredPane: "explorer",
      compactPane: null,
    })).toEqual({
      mode: "single",
      explorerVisible: true,
      dashboardVisible: false,
      editorVisible: true,
    });
  });

  it("shows both requested rails only when their widths leave the editor minimum", () => {
    const exactWideWidth = FILE_EXPLORER_WIDTH + FILE_DASHBOARD_WIDTH + FILE_EDITOR_MIN_WIDTH;
    const input = {
      explorerRequested: true,
      dashboardRequested: true,
      preferredPane: "explorer" as const,
      compactPane: null,
    };

    expect(resolveFilePaneLayout({ ...input, containerWidth: exactWideWidth - 1 }).mode).toBe("single");
    expect(resolveFilePaneLayout({ ...input, containerWidth: exactWideWidth })).toEqual({
      mode: "wide",
      explorerVisible: true,
      dashboardVisible: true,
      editorVisible: true,
    });
  });

  it("honors the requested single-pane priority when either rail can fit", () => {
    const dashboardFirst = resolveFilePaneLayout({
      containerWidth: 900,
      explorerRequested: true,
      dashboardRequested: true,
      preferredPane: "dashboard",
      compactPane: null,
    });
    expect(dashboardFirst.dashboardVisible).toBe(true);
    expect(dashboardFirst.explorerVisible).toBe(false);
    expect(dashboardFirst.editorVisible).toBe(true);

    // The preferred dashboard cannot fit here, so fall back to the narrower
    // requested explorer instead of unnecessarily hiding every side pane.
    const explorerFallback = resolveFilePaneLayout({
      containerWidth: FILE_EXPLORER_WIDTH + FILE_EDITOR_MIN_WIDTH,
      explorerRequested: true,
      dashboardRequested: true,
      preferredPane: "dashboard",
      compactPane: null,
    });
    expect(explorerFallback.explorerVisible).toBe(true);
    expect(explorerFallback.dashboardVisible).toBe(false);
  });

  it("lets an explicitly selected wider pane replace the editor when only the narrower rail fits", () => {
    expect(resolveFilePaneLayout({
      containerWidth: FILE_EXPLORER_WIDTH + FILE_EDITOR_MIN_WIDTH,
      explorerRequested: true,
      dashboardRequested: true,
      preferredPane: "dashboard",
      compactPane: "dashboard",
    })).toEqual({
      mode: "compact",
      explorerVisible: false,
      dashboardVisible: true,
      editorVisible: false,
    });
  });

  it("uses a full-workspace side pane when no fixed rail can coexist with the editor", () => {
    const narrowWidth = FILE_EXPLORER_WIDTH + FILE_EDITOR_MIN_WIDTH - 1;
    const base = {
      containerWidth: narrowWidth,
      explorerRequested: true,
      dashboardRequested: true,
      preferredPane: "explorer" as const,
    };

    expect(resolveFilePaneLayout({ ...base, compactPane: null })).toEqual({
      mode: "compact",
      explorerVisible: false,
      dashboardVisible: false,
      editorVisible: true,
    });
    expect(resolveFilePaneLayout({ ...base, compactPane: "explorer" })).toEqual({
      mode: "compact",
      explorerVisible: true,
      dashboardVisible: false,
      editorVisible: false,
    });
    expect(resolveFilePaneLayout({ ...base, compactPane: "dashboard" })).toEqual({
      mode: "compact",
      explorerVisible: false,
      dashboardVisible: true,
      editorVisible: false,
    });
  });

  it("never reveals a compact pane whose persisted preference is closed", () => {
    expect(resolveFilePaneLayout({
      containerWidth: 500,
      explorerRequested: false,
      dashboardRequested: true,
      preferredPane: "dashboard",
      compactPane: "explorer",
    })).toEqual({
      mode: "compact",
      explorerVisible: false,
      dashboardVisible: false,
      editorVisible: true,
    });
  });

  it("fails safely to an editor-only layout before a finite width is measured", () => {
    for (const containerWidth of [0, -1, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(resolveFilePaneLayout({
        containerWidth,
        explorerRequested: true,
        dashboardRequested: true,
        preferredPane: "explorer",
        compactPane: "explorer",
      })).toEqual({
        mode: "unmeasured",
        explorerVisible: false,
        dashboardVisible: false,
        editorVisible: true,
      });
    }
  });
});
