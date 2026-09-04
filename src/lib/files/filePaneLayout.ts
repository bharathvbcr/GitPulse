export const FILE_EXPLORER_WIDTH = 288;
export const FILE_DASHBOARD_WIDTH = 320;
export const FILE_EDITOR_MIN_WIDTH = 420;

export type FileSidePane = "explorer" | "dashboard";
export type FilePaneLayoutMode = "unmeasured" | "wide" | "single" | "compact";

export interface FilePaneLayoutInput {
  containerWidth: number;
  explorerRequested: boolean;
  dashboardRequested: boolean;
  /** The side pane most recently requested by the user at an intermediate width. */
  preferredPane: FileSidePane;
  /** A side pane temporarily occupying the workspace when no rail can fit. */
  compactPane: FileSidePane | null;
}

export interface FilePaneLayout {
  mode: FilePaneLayoutMode;
  explorerVisible: boolean;
  dashboardVisible: boolean;
  editorVisible: boolean;
}

const editorOnly = (mode: "unmeasured" | "compact"): FilePaneLayout => ({
  mode,
  explorerVisible: false,
  dashboardVisible: false,
  editorVisible: true,
});

/**
 * Keep the editor usable before allocating either fixed-width side pane.
 * User preferences are inputs, not mutations: resizing never overwrites them.
 */
export function resolveFilePaneLayout(input: FilePaneLayoutInput): FilePaneLayout {
  const {
    containerWidth,
    explorerRequested,
    dashboardRequested,
    preferredPane,
    compactPane,
  } = input;
  if (!Number.isFinite(containerWidth) || containerWidth <= 0) {
    return editorOnly("unmeasured");
  }

  const requestedWidth =
    (explorerRequested ? FILE_EXPLORER_WIDTH : 0) +
    (dashboardRequested ? FILE_DASHBOARD_WIDTH : 0);
  if (containerWidth >= FILE_EDITOR_MIN_WIDTH + requestedWidth) {
    return {
      mode: "wide",
      explorerVisible: explorerRequested,
      dashboardVisible: dashboardRequested,
      editorVisible: true,
    };
  }

  const canFitExplorer =
    explorerRequested && containerWidth >= FILE_EDITOR_MIN_WIDTH + FILE_EXPLORER_WIDTH;
  const canFitDashboard =
    dashboardRequested && containerWidth >= FILE_EDITOR_MIN_WIDTH + FILE_DASHBOARD_WIDTH;
  const compactPaneRequested = compactPane === "explorer"
    ? explorerRequested
    : compactPane === "dashboard" ? dashboardRequested : false;
  const compactPaneCanFit = compactPane === "explorer" ? canFitExplorer : canFitDashboard;
  if (compactPane && compactPaneRequested && !compactPaneCanFit) {
    return {
      mode: "compact",
      explorerVisible: compactPane === "explorer",
      dashboardVisible: compactPane === "dashboard",
      editorVisible: false,
    };
  }
  const preferenceOrder: readonly FileSidePane[] =
    preferredPane === "explorer" ? ["explorer", "dashboard"] : ["dashboard", "explorer"];
  const singlePane = preferenceOrder.find((pane) =>
    pane === "explorer" ? canFitExplorer : canFitDashboard,
  );
  if (singlePane) {
    return {
      mode: "single",
      explorerVisible: singlePane === "explorer",
      dashboardVisible: singlePane === "dashboard",
      editorVisible: true,
    };
  }

  if (!compactPane || !compactPaneRequested) return editorOnly("compact");
  return {
    mode: "compact",
    explorerVisible: compactPane === "explorer",
    dashboardVisible: compactPane === "dashboard",
    editorVisible: false,
  };
}
