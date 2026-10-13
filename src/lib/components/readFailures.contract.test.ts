// ft-038e6ba5: wiring that has no mountable harness, pinned at the source.
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const read = (name: string) => readFileSync(join(here, name), "utf8");

describe("ft-038e6ba5 wiring", () => {
  it("a typed answer withdraws the notifier's alert", () => {
    expect(read("TerminalSession.svelte")).toContain("sessionActivity.input(nativeSessionId, data)) reportAttentionAnswered(nativeSessionId)");
  });

  it("a failed notification read can be dismissed and clears on the next good read", () => {
    const bridge = read("NativeNotificationBridge.svelte");
    expect(bridge).toContain('onclick={() => { error = ""; }}>Dismiss');
    expect(bridge).toMatch(/pendingNotificationActivation\(\);\s*if \(disposed\) return;[^]*?error = "";\s*if \(!activation\) return;/);
  });

  it("an unknown or failed holder read forces a worktree and says so", () => {
    const form = read("TaskHandoffForm.svelte");
    expect(form).toContain('const useWorktree = $derived(provisionWorktree || holder.kind !== "free");');
    expect(form).toContain('disabled={locked || holder.kind !== "free"}');
    expect(form).toContain("checkout-holder-unknown");
    expect(form).not.toContain("holding = null;\n    }");
  });

  it("Launch at login renders unknown distinctly from Off", () => {
    const modal = read("SettingsModal.svelte");
    expect(modal).toContain('launchAtLogin.state === "unknown"');
    expect(modal).not.toMatch(/launchAtLogin = false/);
  });
});
