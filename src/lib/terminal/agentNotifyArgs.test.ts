/**
 * The flags GitPulse adds so a launched agent CLI will speak to this terminal.
 *
 * Each one is that CLI's own documented, session-scoped override. What these
 * tests protect is not the spelling — that comes from the vendor's docs — but
 * the three ways the spelling can be right and the launch still wrong: a flag
 * placed after a positional prompt, a flag invented for a CLI that has none,
 * and a flag that arrives when the user turned the setting off.
 */
import { describe, expect, it } from "vitest";
import { agentNotifyArgs, agentPromptArgs, AGENT_NOTIFY_ARGS } from "./launchRequests";
import { LAUNCHERS, type LauncherKind } from "./tabs";

/** What `TerminalSession.launcherConfig` builds. */
function argv(kind: LauncherKind, prompt: string | undefined, configure: boolean): string[] {
  return [...agentNotifyArgs(kind, configure), ...(agentPromptArgs(kind, prompt) ?? [])];
}

describe("agent notification arguments", () => {
  it("gives Claude Code a session-only settings override", () => {
    const args = agentNotifyArgs("claude", true);
    expect(args).toEqual(["--settings", '{"preferredNotifChannel":"terminal_bell"}']);
    // Valid JSON naming exactly one key: `--settings` merges key by key, so a
    // second key here would silently override something the user set.
    const parsed = JSON.parse(args[1]) as Record<string, unknown>;
    expect(Object.keys(parsed)).toEqual(["preferredNotifChannel"]);
  });

  it("gives Codex TOML overrides, quoted as TOML and not as shell", () => {
    const args = agentNotifyArgs("codex", true);
    expect(args.filter((arg) => arg === "-c")).toHaveLength(3);
    expect(args).toContain("tui.notifications=true");
    // The inner quotes are part of the TOML string. These are argv entries; no
    // shell expands them, so stripping them would hand Codex a bare word.
    expect(args).toContain('tui.notification_method="osc9"');
    expect(args).toContain('tui.notification_condition="always"');
  });

  it("invents nothing for a CLI whose notification setting we have not read", () => {
    for (const kind of ["manvi", "grok", "agy", "shell"] as const) {
      expect(agentNotifyArgs(kind, true)).toEqual([]);
    }
  });

  it("adds nothing at all when the setting is off", () => {
    for (const { kind } of LAUNCHERS) {
      expect(agentNotifyArgs(kind, false)).toEqual([]);
    }
  });

  it("hands back a copy, so a caller cannot edit the shared table", () => {
    const first = agentNotifyArgs("claude", true);
    first.push("--dangerously-skip-permissions");
    expect(agentNotifyArgs("claude", true)).not.toContain("--dangerously-skip-permissions");
    expect(AGENT_NOTIFY_ARGS.claude).not.toContain("--dangerously-skip-permissions");
  });

  it("puts every flag before a positional prompt", () => {
    // `claude -- <prompt>` makes everything after `--` positional, so a flag
    // appended behind it becomes part of what the user asked for.
    const args = argv("claude", "Fix the failing test", true);
    const separator = args.indexOf("--");
    expect(separator).toBeGreaterThan(-1);
    expect(args.slice(0, separator)).toContain("--settings");
    expect(args.slice(separator + 1)).toEqual(["Fix the failing test"]);
  });

  it("leaves Antigravity's own prompt flag last", () => {
    const args = argv("agy", "Look at this", true);
    expect(args).toEqual(["--prompt-interactive", "Look at this"]);
  });

  it("a promptless agent launch is flags only", () => {
    expect(argv("codex", undefined, true)).toEqual(agentNotifyArgs("codex", true));
    expect(argv("codex", undefined, false)).toEqual([]);
  });

  it("no flag can be mistaken for a prompt or a path", () => {
    for (const { kind } of LAUNCHERS) {
      for (const arg of agentNotifyArgs(kind, true)) {
        expect(arg).not.toContain("\0");
        expect(arg.includes("\n")).toBe(false);
      }
    }
  });

  it("only names launchers the tab strip actually offers", () => {
    const offered = new Set(LAUNCHERS.map((launcher) => launcher.kind));
    for (const kind of Object.keys(AGENT_NOTIFY_ARGS)) {
      expect(offered.has(kind as LauncherKind)).toBe(true);
    }
  });
});
