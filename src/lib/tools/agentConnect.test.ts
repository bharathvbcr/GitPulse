import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
vi.mock("../ipc/invoke", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));

import {
  applyAgentConnect,
  buildSetupPrompt,
  canConnect,
  connectConfirmText,
  connectSummary,
  getAgentConnectPlan,
  type ConnectAction,
  type ConnectPlan,
} from "./agentConnect";
import { buildDevcouncilInstallCommand } from "./devcouncilInstall";

const MCP = "/Applications/GitPulse.app/Contents/MacOS/gitpulse-mcp";
const HOOK = "/Applications/GitPulse.app/Contents/MacOS/gitpulse-hook";

function action(over: Partial<ConnectAction>): ConnectAction {
  return {
    id: "link:gitpulse-mcp",
    kind: "link",
    agent: null,
    title: "Link gitpulse-mcp into ~/.local/bin",
    target: "/Users/me/.local/bin/gitpulse-mcp",
    state: "todo",
    detail: "",
    ...over,
  };
}

function plan(over: Partial<ConnectPlan> = {}): ConnectPlan {
  return {
    available: true,
    reason: "",
    mcp_path: MCP,
    hook_path: HOOK,
    agents_found: ["claude", "codex"],
    agent_commands: [
      {
        agent: "claude",
        label: "Claude Code",
        add: `claude mcp add --scope user gitpulse -- ${MCP}`,
        verify: "claude mcp list",
      },
      {
        agent: "codex",
        label: "Codex",
        add: `codex mcp add gitpulse -- ${MCP}`,
        verify: "codex mcp list",
      },
    ],
    actions: [action({})],
    ...over,
  };
}

beforeEach(() => invoke.mockReset());

describe("connect client", () => {
  it("plans and applies through the two backend commands", async () => {
    invoke.mockResolvedValueOnce(plan()).mockResolvedValueOnce({ results: [], plan: plan() });
    await getAgentConnectPlan();
    await applyAgentConnect();
    expect(invoke.mock.calls.map((c) => c[0])).toEqual([
      "cmd_agent_connect_plan",
      "cmd_agent_connect_apply",
    ]);
  });
});

describe("setup prompt", () => {
  it("names the absolute bundled paths, what to register and how to verify", () => {
    const text = buildSetupPrompt(plan());
    expect(text).toContain(MCP);
    expect(text).toContain(HOOK);
    expect(text).toContain(`claude mcp add --scope user gitpulse -- ${MCP}`);
    expect(text).toContain(`codex mcp add gitpulse -- ${MCP}`);
    expect(text).toContain("claude mcp list");
    expect(text).toContain("codex mcp list");
    expect(text).toContain("never overwrite");
  });

  it("uses the install command the app already builds, and never the checkout-only script", () => {
    const devmap = buildDevcouncilInstallCommand({ preset: "devmap", sourceCheckout: null });
    const text = buildSetupPrompt(plan());
    expect(text).toContain(devmap.command);
    expect(text).not.toContain("mcp:install");
    expect(text).not.toContain("bun run");
  });

  it("tells a machine without the bundled binary to reinstall, not to build from source", () => {
    const text = buildSetupPrompt(
      plan({ available: false, mcp_path: "", hook_path: "", agent_commands: [], actions: [] }),
    );
    expect(text).not.toContain("mcp:install");
    expect(text).not.toContain("cargo build");
    expect(text).toContain("reinstall GitPulse");
  });

  it("quotes a path with a quote or space in the link command", () => {
    const text = buildSetupPrompt(plan({ mcp_path: "/My Apps/it's/gitpulse-mcp" }));
    expect(text).toContain("ln -s '/My Apps/it'\\''s/gitpulse-mcp'");
  });
});

describe("plan wording", () => {
  it("offers Connect only when something would change", () => {
    expect(canConnect(plan())).toBe(true);
    expect(canConnect(plan({ actions: [action({ state: "done" })] }))).toBe(false);
    expect(canConnect(plan({ actions: [action({ state: "blocked" })] }))).toBe(false);
    expect(canConnect(plan({ available: false }))).toBe(false);
  });

  it("does not call a blocked plan connected", () => {
    expect(connectSummary(plan({ actions: [action({ state: "blocked" })] }))).toContain("left alone");
    expect(connectSummary(plan({ actions: [action({ state: "done" })] }))).toBe("Connected. Nothing to change.");
    expect(connectSummary(plan({ available: false, reason: "no binary" }))).toBe("no binary");
  });

  it("confirms with every pending action and its path, and skips the ones left alone", () => {
    const text = connectConfirmText(
      plan({
        actions: [
          action({}),
          action({ id: "register:claude", title: "Register with Claude Code", target: "claude mcp add x" }),
          action({ id: "link:gitpulse-hook", title: "Link hook", target: "/h", state: "blocked" }),
        ],
      }),
    );
    expect(text).toContain("/Users/me/.local/bin/gitpulse-mcp");
    expect(text).toContain("claude mcp add x");
    expect(text).not.toContain("Link hook");
  });
});

describe("connect UI wiring", () => {
  const here = dirname(fileURLToPath(import.meta.url));
  const read = (rel: string) => readFileSync(join(here, "..", rel), "utf8");

  it("asks through the in-app dialog before applying, and offers the prompt", () => {
    const panel = read("components/AgentConnectPanel.svelte");
    expect(panel).toContain("await askConfirm(");
    expect(panel).not.toContain("window.confirm");
    expect(panel).toContain("Copy setup prompt");
    expect(panel).toContain("buildSetupPrompt(plan");
  });

  it("is reachable from Settings, the first-run card and the wizard's done step", () => {
    expect(read("components/SettingsModal.svelte")).toContain("<AgentConnectPanel");
    expect(read("../App.svelte")).toContain('openSetupWizard("devmap", "connect")');
    const wizard = read("components/onboarding/SetupWizard.svelte");
    expect(wizard).toContain("<AgentConnectPanel");
    expect(wizard).toMatch(/Connect your agent[\s\S]*Done/);
  });

  it("is registered on both sides of the IPC seam", () => {
    const lib = readFileSync(join(here, "../../../src-tauri/src/lib.rs"), "utf8");
    expect(lib).toContain("cmd_agent_connect_plan,");
    expect(lib).toContain("cmd_agent_connect_apply,");
  });
});
