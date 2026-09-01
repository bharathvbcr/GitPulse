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
    expect(workflow).toContain("releaseBody: ${{ env.RELEASE_NOTES }}");
    // the old block described v0.0.3 whatever tag was being built
    expect(workflow).not.toContain("GitPulse v__VERSION__ introduces");
  });
});
