/**
 * Connect an installed agent (Claude Code, Codex) to GitPulse's MCP server.
 *
 * The backend builds a plan with no side effects and applies it only after the
 * user confirmed the one shown. This module is the IPC seam, the wording of
 * that consent, and the self-contained prompt a user can paste into their own
 * agent instead of clicking Connect.
 */
import { invoke } from "../ipc/invoke";
import { buildDevcouncilInstallCommand } from "./devcouncilInstall";

export type ConnectActionKind = "link" | "register";
export type ConnectActionState = "todo" | "done" | "blocked";

export interface ConnectAction {
  id: string;
  kind: ConnectActionKind;
  agent: string | null;
  title: string;
  /** The path touched, or the command line that would run. */
  target: string;
  state: ConnectActionState;
  detail: string;
  /**
   * Lines the user can run themselves for what Connect leaves alone: switch a
   * registration or a link, remove an older copy. Connect never runs them.
   */
  commands: string[];
}

export interface AgentCommand {
  agent: string;
  label: string;
  /** The agent's own CLI line that registers the server. */
  add: string;
  /** The agent's own CLI line that lists registrations. */
  verify: string;
}

export interface ConnectPlan {
  available: boolean;
  reason: string;
  mcp_path: string;
  hook_path: string;
  agents_found: string[];
  agent_commands: AgentCommand[];
  actions: ConnectAction[];
}

export interface ConnectResult {
  id: string;
  ok: boolean;
  message: string;
}

export interface ConnectOutcome {
  results: ConnectResult[];
  plan: ConnectPlan;
}

export function getAgentConnectPlan(): Promise<ConnectPlan> {
  return invoke<ConnectPlan>("cmd_agent_connect_plan");
}

export function applyAgentConnect(): Promise<ConnectOutcome> {
  return invoke<ConnectOutcome>("cmd_agent_connect_apply");
}

/** Applying is only offered when something would change. */
export function canConnect(plan: ConnectPlan): boolean {
  return plan.available && plan.actions.some((a) => a.state === "todo");
}

/** The one-line state under the card title. */
export function connectSummary(plan: ConnectPlan): string {
  if (!plan.available) return plan.reason || "GitPulse's agent binaries were not found.";
  const todo = plan.actions.filter((a) => a.state === "todo").length;
  if (todo > 0) return `${todo} change${todo === 1 ? "" : "s"} to make. Nothing is written until you confirm.`;
  const blocked = plan.actions.filter((a) => a.state === "blocked").length;
  if (blocked > 0) return "Nothing to change; some items were left alone (see below).";
  return "Connected. Nothing to change.";
}

/** The confirmation text: every action with its path or command. */
export function connectConfirmText(plan: ConnectPlan): string {
  const lines = ["GitPulse will make these changes:", ""];
  for (const action of plan.actions.filter((a) => a.state === "todo")) {
    lines.push(`- ${action.title}`, `  ${action.target}`);
  }
  lines.push(
    "",
    "Links go in ~/.local/bin; agents are registered through their own CLI. Nothing else is touched.",
  );
  return lines.join("\n");
}

/**
 * Claude Code's plugin commands for GitPulse's marketplace
 * (`.claude-plugin/marketplace.json` at the public repository's root, plugin
 * `gitpulse`). Syntax from `claude plugin marketplace add --help` and
 * `claude plugin install --help`: `<source>` and `<plugin>@<marketplace>`.
 */
export const CLAUDE_PLUGIN_MARKETPLACE_ADD = "claude plugin marketplace add bharathvbcr/GitPulse";
export const CLAUDE_PLUGIN_INSTALL = "claude plugin install gitpulse@gitpulse";

function sh(text: string): string {
  return `'${text.replace(/'/g, `'\\''`)}'`;
}

/**
 * A prompt the user pastes into their own agent. Self-contained: absolute
 * bundled paths, what to register, how to verify. DevMap's install command is
 * the one the setup wizard already builds; Manvi has no command of its own
 * here, so the prompt points at the wizard rather than inventing one.
 */
export function buildSetupPrompt(plan: ConnectPlan, windows = false): string {
  if (!plan.available || !plan.mcp_path) {
    return [
      "Help me connect GitPulse to my coding agents.",
      "",
      "GitPulse couldn't find its bundled gitpulse-mcp binary on this machine, so there is nothing to register yet.",
      "Do not install anything from a source checkout. Tell me to reinstall GitPulse from its release download and try again.",
    ].join("\n");
  }
  const lines: string[] = [
    "Help me connect GitPulse's MCP server to my coding agents. GitPulse is installed as a desktop app; its helper binaries are inside the app bundle and are not on PATH.",
    "",
    "Bundled binaries (absolute paths):",
    `- gitpulse-mcp: ${plan.mcp_path}`,
  ];
  if (plan.hook_path) lines.push(`- gitpulse-hook: ${plan.hook_path}`);
  lines.push(
    "",
    "Steps. Run each one, never overwrite an existing file or link, and stop and tell me if something already exists with different contents:",
    "1. Make the binaries reachable by name. Only if the destination does not already exist:",
    "   mkdir -p ~/.local/bin",
    `   ln -s ${sh(plan.mcp_path)} ~/.local/bin/gitpulse-mcp`,
  );
  if (plan.hook_path) lines.push(`   ln -s ${sh(plan.hook_path)} ~/.local/bin/gitpulse-hook`);
  lines.push(
    "2. Register the server with each agent CLI I have installed, using the agent's own command (do not edit its config files by hand). Check first, and register only what is missing:",
    "   - Claude Code: run `claude plugin list --json`. If an enabled plugin lists a gitpulse server under mcpServers (GitPulse's plugin), skip Claude Code: the plugin already provides the server, and a second registration would duplicate every tool.",
    "   - Run `<agent> mcp get gitpulse`. If gitpulse is already registered with a different command (for example an older `cargo install` copy in ~/.cargo/bin), stop and ask me before replacing it. To replace it: `claude mcp remove gitpulse -s <the scope it shows>` or `codex mcp remove gitpulse`, then the line below.",
  );
  for (const command of plan.agent_commands) {
    lines.push(`   ${command.label}: ${command.add}`);
  }
  lines.push("3. Verify it is registered:");
  for (const command of plan.agent_commands) {
    lines.push(`   ${command.label}: ${command.verify}`);
  }
  lines.push("   The server is named gitpulse. Tell me which agents you registered it with, and which you skipped and why.");
  lines.push(
    "4. Check that ~/.local/bin is on my shell's PATH (echo $PATH). The registration above uses the full path, so the MCP server works either way, but GitPulse's Claude Code plugin and hooks call gitpulse-mcp and gitpulse-hook by name. If it is missing, tell me which shell profile line would add it; do not edit my profile yourself.",
    "5. Check for older copies: if ~/.cargo/bin/gitpulse-mcp or ~/.cargo/bin/gitpulse-hook exists, it is from an earlier source install and may come before ~/.local/bin on PATH. Tell me; `cargo uninstall gitpulse` removes them, but ask me first (I may build GitPulse from source on purpose).",
  );

  lines.push(
    "",
    "Optional, only if I say yes: GitPulse's Claude Code plugin adds its hooks (collision guard, command gate) and skills, and provides the gitpulse server itself. Install it with Claude Code's own plugin commands:",
    `   ${CLAUDE_PLUGIN_MARKETPLACE_ADD}`,
    `   ${CLAUDE_PLUGIN_INSTALL}`,
    "   If you registered gitpulse with Claude Code in step 2, remove that registration afterwards (`claude mcp remove gitpulse -s user`) so there is one server, not two.",
  );
  const devmap = buildDevcouncilInstallCommand({ preset: "devmap", sourceCheckout: null, windows });
  if (devmap.runnable && devmap.command) {
    lines.push(
      "Optional, only if I say yes: install DevCouncil's code map, which GitPulse's Map, Health, Suspects and impact views use. It needs a Rust toolchain (cargo):",
      `   ${devmap.command}`,
    );
  }
  lines.push(
    "Optional, only if I say yes: Manvi (policy checks before commits, pushes and other Git actions) is set up from GitPulse itself: Settings > Agents > Manvi policy > Set up Manvi. Building it needs a Go toolchain.",
  );
  return lines.join("\n");
}
