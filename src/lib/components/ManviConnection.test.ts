import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import { scopedRunnerPresentation } from "../harness/policyPresentation";
import ManviConnection from "./ManviConnection.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "ManviConnection.svelte"),
  "utf8",
);

describe("ManviConnection capability truth", () => {
  it("distinguishes the user PTY from scoped model-authored execution", () => {
    const { body } = render(ManviConnection);
    expect(body).toContain("Manvi policy");
    expect(body).toContain("Capability boundary");
    expect(body).toContain("Interactive shell");
    expect(body).toContain("User only");
    expect(body).toContain("Scoped action runner");
    expect(body).toContain("purpose allowlist");
  });

  it("does not use the retired names", () => {
    const { body } = render(ManviConnection);
    expect(body).not.toContain("Harness & AI");
    expect(body).not.toContain("MANVI harness");
  });

  it("does not claim the embedded sidecar exposes native agent tools", () => {
    expect(source).toContain("policy and local-model planes only");
    expect(source).toContain("No autonomous PTY or app-control API");
  });

  it("describes the scoped runner from the current permission state", () => {
    expect(scopedRunnerPresentation("connected")).toMatchObject({
      label: "Guarded",
      tone: "ready",
    });
    expect(scopedRunnerPresentation("unguarded")).toMatchObject({
      label: "Not checked",
      tone: "warning",
    });
    expect(scopedRunnerPresentation("blocked")).toMatchObject({
      label: "Blocked",
      tone: "error",
    });
    expect(scopedRunnerPresentation("not-probed")).toMatchObject({
      label: "Status unknown",
      tone: "neutral",
    });
    expect(scopedRunnerPresentation("connected", "refresh failed")).toMatchObject({
      label: "Status stale",
      tone: "error",
    });
    expect(source).not.toContain(">Available</span>");
    expect(source).toContain("scopedRunnerPresentation(permissionMode, harnessError)");
  });

  it("links the actual health, coverage, terminal and CI surfaces", () => {
    for (const tab of ["health", "coverage", "github"]) {
      expect(source).toContain(`openCapability("${tab}")`);
    }
    // The terminal is a dock, not a view, so its button opens it over the page
    // instead of navigating away. Still a real link, just not a tab switch.
    expect(source).toContain("onclick={openTerminal}");
    expect(source).toContain("repoStore.setTerminalOpen(true)");
    expect(source).toContain("cargo-llvm-cov");
    expect(source).toContain("several minutes");
  });

  it("closes the dialog it sits in once a shortcut has navigated", () => {
    // Settings is a modal over the view: a shortcut that left it open would
    // change the page behind it where the reader cannot see it.
    for (const fn of ["openCapability", "openTerminal"]) {
      const body = source.slice(source.indexOf(`function ${fn}`));
      expect(body.slice(0, body.indexOf("\n  }\n"))).toContain("onNavigate?.()");
    }
  });

  it("keeps Manvi's install out of this card, which External CLIs already owns", () => {
    // Rendering the install panel here as well would put two copies of the same
    // buttons on one Settings page.
    expect(source).not.toContain("ExternalToolsPanel");
    expect(source).toContain('openSetupWizard("manvi", "explain")');
  });

  it("reports a failed status read and a blocked gate as failures, not as no news", () => {
    expect(source).toContain("Couldn't check Manvi status");
    expect(source).toContain("Policy gate failed — mutations blocked");
    expect(source).toContain("GitPulse will not treat a timed-out");
  });
});
