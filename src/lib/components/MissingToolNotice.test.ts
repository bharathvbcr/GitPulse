import { readFileSync } from "node:fs";
import { beforeEach, describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";
import type { ToolsStatus } from "../tools/externalTools";
import { onboardingStore } from "../tools/onboardingStore";
import MissingToolNotice from "./MissingToolNotice.svelte";

const source = readFileSync(new URL("./MissingToolNotice.svelte", import.meta.url), "utf8");

function tools(devmapInstalled: boolean): ToolsStatus {
  const tool = (installed: boolean) => ({ installed }) as ToolsStatus["devmap"];
  return { devmap: tool(devmapInstalled), manvi: tool(true) };
}

function html(props: Record<string, unknown>): string {
  return render(MissingToolNotice, {
    props: { repoPath: "/repo", feature: "symbol search", ...props },
  }).body;
}

describe("MissingToolNotice", () => {
  beforeEach(() => onboardingStore.status.set(null));

  it("compiles without warnings", () => {
    const { warnings } = compile(source, { generate: "client", filename: "MissingToolNotice.svelte" });
    expect(warnings).toEqual([]);
  });

  it("missing CLI: says what is missing and what Setup gives, with a Set up button", () => {
    onboardingStore.status.set(tools(false));
    const body = html({ graph: { available: false } });
    expect(body).toContain('data-gap="cli_missing"');
    expect(body).toContain("The code map tool is not installed.");
    expect(body).toContain("to get symbol search");
    expect(body).toContain("Set up code map");
    expect(body).not.toContain("Build index");
  });

  it("CLI installed but no index: offers Build index, not Setup", () => {
    onboardingStore.status.set(tools(true));
    const body = html({ graph: { available: false } });
    expect(body).toContain('data-gap="no_index"');
    expect(body).toContain("Build index");
    expect(body).not.toContain("Set up code map");
  });

  it("index present: renders no notice and no install prompt", () => {
    onboardingStore.status.set(tools(false));
    const body = html({ graph: { available: true } });
    expect(body).not.toContain("gp-notice");
    expect(body).not.toContain("<button");
  });

  it("another failure: shows the surface's honest text and no install CTA", () => {
    onboardingStore.status.set(tools(false));
    const body = html({ graph: { available: true }, otherText: "Suspects unavailable — not the same as no suspects." });
    expect(body).not.toContain("<button");
    const pending = html({ otherText: "Suspects unavailable — not the same as no suspects." });
    expect(pending).toContain("not the same as no suspects");
    expect(pending).not.toContain("Set up code map");
    expect(pending).not.toContain("Build index");
  });

  it("another failure with no text of its own renders nothing", () => {
    expect(html({})).not.toContain("<button");
    expect(html({})).not.toContain("gp-notice");
  });

  it("keeps the raw reason in a tooltip rather than printing it", () => {
    onboardingStore.status.set(tools(true));
    const body = html({ graph: { available: false }, reason: "no such file: /x/devmap.sqlite" });
    expect(body).toContain('title="no such file: /x/devmap.sqlite"');
    expect(body).not.toContain(">no such file");
  });

  it("opens the setup wizard on the devmap tool, after the host's own cleanup", () => {
    expect(source).toContain("await onBeforeSetup?.();");
    expect(source).toContain('openSetupWizard("devmap", "explain")');
  });
});
