import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import ManviHarnessPane from "./ManviHarnessPane.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "ManviHarnessPane.svelte"),
  "utf8",
);

describe("ManviHarnessPane capability truth", () => {
  it("distinguishes the user PTY from scoped model-authored execution", () => {
    const { body } = render(ManviHarnessPane);
    expect(body).toContain("Capability boundary");
    expect(body).toContain("Interactive shell");
    expect(body).toContain("User only");
    expect(body).toContain("Scoped action runner");
    expect(body).toContain("purpose allowlist");
  });

  it("does not claim the embedded sidecar exposes native agent tools", () => {
    expect(source).toContain("policy and local-model planes only");
    expect(source).toContain("No autonomous PTY or app-control API");
  });

  it("links the actual health, coverage, terminal and CI surfaces", () => {
    for (const tab of ["health", "coverage", "terminal", "github"]) {
      expect(source).toContain(`openCapability("${tab}")`);
    }
    expect(source).toContain("cargo-llvm-cov");
    expect(source).toContain("several minutes");
  });
});
