import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const workflow = readFileSync(new URL("../.github/workflows/release.yml", import.meta.url), "utf8");

describe("release workflow contracts", () => {
  it("runs every repository contract gate before building release assets", () => {
    const preflight = workflow.slice(workflow.indexOf("preflight:"), workflow.indexOf("\n  release:"));
    expect(preflight).toContain("run: npm run check:ipc");
    expect(preflight).toContain("run: npm run check:types");
    expect(preflight).toContain("run: npm run check:release");
  });

  it("uses exact asset-manifest verification after the matrix completes", () => {
    expect(workflow).toContain("node scripts/check-release-assets.mjs");
    expect(workflow).not.toContain("grep -qE");
  });

  it("takes the release body from the changelog, not a literal block", () => {
    expect(workflow).toContain("node scripts/release-notes.mjs --tag");
    expect(workflow).toContain("gh release edit");
    expect(workflow).toContain("--notes-file");
    // the old block described v0.0.3 whatever tag was being built
    expect(workflow).not.toContain("GitPulse v__VERSION__ introduces");
  });

  it("does not funnel changelog notes through GITHUB_ENV", () => {
    // v0.0.5 notes are 54 KB; GitHub caps a GITHUB_ENV variable at 48 KB.
    // Writing them there fails the job after the binaries have already built.
    expect(workflow).not.toContain("RELEASE_NOTES<<");
    expect(workflow).not.toContain("${{ env.RELEASE_NOTES }}");
  });

  it("fails preflight when the tag has no changelog section", () => {
    const preflight = workflow.slice(workflow.indexOf("preflight:"), workflow.indexOf("\n  release:"));
    expect(preflight).toContain("run: npm run release:notes -- --tag \"$RELEASE_TAG\"");
  });

  it("uses tauri-action v1's uploadUpdaterJson input, not the v0 name", () => {
    // v1 renamed includeUpdaterJson; the old key is ignored and the default
    // (true) would start publishing latest.json without a decision.
    expect(workflow).toContain("uploadUpdaterJson: false");
    expect(workflow).not.toMatch(/^\s*includeUpdaterJson:/m);
  });
});
