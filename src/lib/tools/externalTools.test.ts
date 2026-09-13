import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { askConfirm } from "../stores/modalStore";
import { autoInit } from "../codeintel/autoInit";
import {
  cancelExternalToolInstall,
  classifyMapFailure,
  cloneToolSource,
  getExternalToolsStatus,
  getToolConfig,
  getToolLadder,
  getToolPreflight,
  installButtonLabel,
  installExternalTool,
  mapFailureMessage,
  refreshToolCapability,
  saveToolConfig,
  setExternalToolDisabled,
  toolStatusSummary,
  uninstallExternalTool,
  verifyTool,
  type ToolStatus,
} from "./externalTools";

vi.mock("../stores/modalStore", () => ({ askConfirm: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

function status(partial: Partial<ToolStatus>): ToolStatus {
  return {
    tool: "devmap",
    installed: false,
    path: null,
    lookup: "missing",
    version: null,
    reason: null,
    source_checkout: null,
    install_ready: false,
    install_block: null,
    install_command: "cargo install --path …",
    ...partial,
  };
}

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(askConfirm).mockReset().mockResolvedValue(false);
});

describe("toolStatusSummary", () => {
  it("names a broken explicit path rather than 'not installed'", () => {
    expect(
      toolStatusSummary(
        status({
          lookup: "explicit_missing",
          reason: "GITPULSE_DEVMAP_BIN is set to /x, but that path is not a file",
        }),
      ),
    ).toContain("GITPULSE_DEVMAP_BIN");
  });

  it("shows the path that answered when installed", () => {
    expect(
      toolStatusSummary(
        status({
          installed: true,
          path: "/Users/me/.cargo/bin/devmap",
          lookup: "path_search",
          version: "devmap 1.2.3",
        }),
      ),
    ).toContain("/Users/me/.cargo/bin/devmap");
  });

  it("labels a saved_config lookup", () => {
    expect(
      toolStatusSummary(
        status({
          installed: true,
          path: "/app/bin/devmap",
          lookup: "saved_config",
        }),
      ),
    ).toContain("saved config");
  });

  it("surfaces stale config rather than silent drop", () => {
    expect(
      toolStatusSummary(
        status({
          stale_config: "/old/devmap no longer exists",
        }),
      ),
    ).toContain("Stale saved path");
  });

  it("falls back to a not-installed reason", () => {
    expect(toolStatusSummary(status({ reason: null }))).toContain("not installed");
  });

  it("names a disabled tool", () => {
    expect(
      toolStatusSummary(
        status({
          disabled: true,
          reason: "devmap is disabled in GitPulse settings",
        }),
      ),
    ).toContain("disabled");
  });
});

describe("installButtonLabel", () => {
  it("says Update when already installed", () => {
    expect(installButtonLabel(status({ installed: true }))).toBe("Update devmap");
  });

  it("says Install when missing", () => {
    expect(installButtonLabel(status({ tool: "manvi" }))).toBe("Install manvi");
  });
});

describe("classifyMapFailure", () => {
  it("separates no CLI from missing sqlite and missing JSON", () => {
    expect(
      classifyMapFailure({
        cliAvailable: false,
        mapAvailable: false,
        mapReason: "whatever",
      }),
    ).toBe("no_cli");

    expect(
      classifyMapFailure({
        cliAvailable: true,
        mapAvailable: false,
        mapReason: "no map at .devcouncil/codeintel/devmap.sqlite",
      }),
    ).toBe("no_sqlite_store");

    expect(
      classifyMapFailure({
        cliAvailable: true,
        mapAvailable: false,
        mapReason: "index has not been built",
      }),
    ).toBe("no_sqlite_store");

    expect(
      classifyMapFailure({
        cliAvailable: true,
        mapAvailable: false,
        mapReason: "no repo map at /repo/.devcouncil/repo_map.json; run Build Map first",
        mapPath: "/repo/.devcouncil/repo_map.json",
      }),
    ).toBe("no_json_artifact");

    expect(
      classifyMapFailure({
        cliAvailable: true,
        mapAvailable: false,
        mapReason: "something else",
        mapPath: "/repo/.devcouncil/repo_map.json",
      }),
    ).toBe("no_json_artifact");

    expect(
      classifyMapFailure({
        cliAvailable: true,
        mapAvailable: false,
        mapReason: "unknown",
      }),
    ).toBe("other");

    expect(
      classifyMapFailure({
        cliAvailable: true,
        mapAvailable: true,
      }),
    ).toBe("other");
  });

  it("mapFailureMessage names the action for each mode", () => {
    expect(mapFailureMessage("no_cli")).toContain("Setup");
    expect(mapFailureMessage("no_sqlite_store")).toContain("Build");
    expect(mapFailureMessage("no_json_artifact")).toContain("repo_map.json");
    expect(mapFailureMessage("other", "custom")).toBe("custom");
    expect(mapFailureMessage("other")).toBe("Map unavailable");
  });
});

describe("IPC wrappers", () => {
  it("invokes status, install, cancel, config, ladder, preflight, verify, clone, refresh", async () => {
    vi.mocked(invoke).mockResolvedValue({});

    await getExternalToolsStatus();
    expect(invoke).toHaveBeenCalledWith("cmd_external_tools_status");

    await installExternalTool("devmap", "toolchain_remote");
    expect(invoke).toHaveBeenCalledWith("cmd_external_tool_install", {
      tool: "devmap",
      rung: "toolchain_remote",
    });

    await installExternalTool("manvi");
    expect(invoke).toHaveBeenCalledWith("cmd_external_tool_install", {
      tool: "manvi",
      rung: null,
    });

    await cancelExternalToolInstall();
    expect(invoke).toHaveBeenCalledWith("cmd_external_tool_install_cancel");

    await uninstallExternalTool("devmap");
    expect(invoke).toHaveBeenCalledWith("cmd_external_tool_uninstall", { tool: "devmap" });

    await setExternalToolDisabled("devmap", true);
    expect(invoke).toHaveBeenCalledWith("cmd_external_tool_set_disabled", {
      tool: "devmap",
      disabled: true,
    });

    await getToolConfig();
    expect(invoke).toHaveBeenCalledWith("cmd_tool_config_get");

    const cfg = {
      version: 1,
      devmap: {},
      manvi: {},
      onboarding: { skipped_tools: [] as string[], dismissed: false },
    };
    await saveToolConfig(cfg);
    expect(invoke).toHaveBeenCalledWith("cmd_tool_config_save", { config: cfg });

    await getToolLadder("devmap");
    expect(invoke).toHaveBeenCalledWith("cmd_tool_ladder", { tool: "devmap" });

    await getToolPreflight("manvi");
    expect(invoke).toHaveBeenCalledWith("cmd_tool_preflight", { tool: "manvi" });

    await verifyTool("devmap");
    expect(invoke).toHaveBeenCalledWith("cmd_tool_verify", { tool: "devmap" });

    await cloneToolSource("devmap", "/tmp");
    expect(invoke).toHaveBeenCalledWith("cmd_onboarding_clone_source", {
      tool: "devmap",
      parentDir: "/tmp",
    });

    await refreshToolCapability();
    expect(invoke).toHaveBeenCalledWith("cmd_tool_capability_refresh");
  });
});


describe("source checkout trust", () => {
  const denied = {
    tool: "manvi", ok: false, binary: null, lookup: null, version: null,
    source_used: "/source", command: "go -C /source install ./cmd/manvi", exit_code: null,
    stdout: "", stderr: "", timed_out: false, cancelled: false,
    reason: "REPOSITORY_TRUST_REQUIRED: /source", rung: "local_checkout",
  };
  const preview = { path: "/source", git_dir: "/source/.git", common_dir: "/source/.git", identity: "native-identity", trusted: false };
  it("approves the exact native source identity before one bounded retry", async () => {
    vi.mocked(askConfirm).mockResolvedValue(true);
    vi.mocked(invoke).mockResolvedValueOnce(denied).mockResolvedValueOnce(preview)
      .mockResolvedValueOnce(undefined).mockResolvedValueOnce({ ...denied, ok: true, reason: null });
    expect((await installExternalTool("manvi", "local_checkout")).ok).toBe(true);
    expect(invoke).toHaveBeenNthCalledWith(3, "cmd_grant_repository_trust", {
      repoPath: "/source", expectedIdentity: "native-identity",
    });
    expect(invoke).toHaveBeenNthCalledWith(4, "cmd_external_tool_install", { tool: "manvi", rung: "local_checkout" });
    expect(askConfirm).toHaveBeenCalledTimes(1);
  });
  it("cancels without granting or retrying when source trust is declined", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(denied).mockResolvedValueOnce(preview);
    const result = await installExternalTool("manvi", "local_checkout");
    expect(result.cancelled).toBe(true);
    expect(invoke).toHaveBeenCalledTimes(2);
  });
});

describe("the tools probe as an invalidation signal", () => {
  // Per-repository initialization skips the workspace registry when devmap is
  // absent and remembers that skip. This probe is the only place the app ever
  // learns devmap appeared — from its own installer or from a terminal — so it
  // is the only thing that can un-stick those repositories.
  it("tells auto-init when devmap is present", async () => {
    const notify = vi.spyOn(autoInit, "onToolsChanged").mockImplementation(() => {});
    vi.mocked(invoke).mockResolvedValue({
      devmap: status({ tool: "devmap", installed: true, path: "/usr/local/bin/devmap" }),
      manvi: status({ tool: "manvi" }),
    });
    await getExternalToolsStatus();
    expect(notify).toHaveBeenCalledTimes(1);
    notify.mockRestore();
  });

  it("stays silent while devmap is still missing", async () => {
    // Nothing changed, and re-running initialization on every probe would be
    // pure cost for a repository that is correctly waiting on an install.
    const notify = vi.spyOn(autoInit, "onToolsChanged").mockImplementation(() => {});
    vi.mocked(invoke).mockResolvedValue({
      devmap: status({ tool: "devmap", installed: false }),
      manvi: status({ tool: "manvi", installed: true }),
    });
    await getExternalToolsStatus();
    expect(notify).not.toHaveBeenCalled();
    notify.mockRestore();
  });

  it("does not throw on a payload with no devmap entry", async () => {
    const notify = vi.spyOn(autoInit, "onToolsChanged").mockImplementation(() => {});
    vi.mocked(invoke).mockResolvedValue({});
    await expect(getExternalToolsStatus()).resolves.toEqual({});
    expect(notify).not.toHaveBeenCalled();
    notify.mockRestore();
  });
});
