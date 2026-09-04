import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import type { HarnessStatus } from "../stores/harnessStore";
import {
  harnessPermissionMode,
  harnessPermissionSummary,
} from "./availability";

function status(
  overrides: Partial<HarnessStatus>,
): HarnessStatus {
  return {
    available: false,
    binary: "",
    protocol: 0,
    posture: "",
    ops: [],
    error: "",
    error_code: "",
    ...overrides,
  };
}

describe("harness permission availability", () => {
  it("permits unchecked mutations only when MANVI is not installed", () => {
    expect(
      harnessPermissionMode(status({ error_code: "not_installed" })),
    ).toBe("unguarded");

    for (const error_code of [
      "unavailable",
      "timeout",
      "protocol",
      "busy",
      "refused",
    ]) {
      expect(harnessPermissionMode(status({ error_code }))).toBe("blocked");
    }
  });

  it("keeps connected and not-yet-probed states distinct", () => {
    expect(harnessPermissionMode(null)).toBe("not-probed");
    expect(harnessPermissionMode(status({ available: true }))).toBe("connected");
  });

  it("never tells users a failing policy gate is unchecked mode", () => {
    const summary = harnessPermissionSummary(
      status({ error_code: "timeout", error: "handshake timed out" }),
    );
    expect(summary).toContain("blocked");
    expect(summary).not.toContain("still work");
    expect(summary).not.toContain("unchecked mode");
  });

  it("routes every MANVI status surface through the same permission truth", () => {
    const components = [
      "../components/HarnessBadge.svelte",
      "../components/ManviHarnessPane.svelte",
      "../components/ManviOpsPanel.svelte",
    ].map((path) => readFileSync(new URL(path, import.meta.url), "utf8"));

    for (const source of components) {
      expect(source).toContain("harnessPermissionMode");
    }
    expect(components.join("\n")).not.toContain("GitPulse still works");
    expect(components.join("\n")).not.toContain("unchecked mode");
  });
});
