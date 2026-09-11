import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const stateScript = readFileSync(new URL("./release-state.mjs", import.meta.url), "utf8");
const workflow = readFileSync(new URL("../.github/workflows/release.yml", import.meta.url), "utf8");

describe("release workflow contracts", () => {
  it("runs every repository contract gate before building release assets", () => {
    const preflight = workflow.slice(workflow.indexOf("preflight:"), workflow.indexOf("\n  release:"));
    expect(preflight).toContain("run: npm run check:ipc");
    expect(preflight).toContain("run: npm run check:types");
    expect(preflight).toContain("run: npm run check:release");
  });

  it("uses exact asset-manifest verification after the matrix completes", () => {
    expect(stateScript).toContain("inspectReleaseAssets({ tag, json: release })");
    expect(workflow).not.toContain("grep -qE");
  });

  it("takes the release body from the changelog, not a literal block", () => {
    expect(workflow).toContain("npm run release:notes -- --tag");
    expect(stateScript).toContain('extractNotes(readFileSync("CHANGELOG.md", "utf8"), tag)');
    expect(stateScript).toContain("confirmed.body !== notes");
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

it("pins all build and verification jobs to the preflight commit", () => {
  const builds = workflow.slice(workflow.indexOf("\n  release:"));
  expect(builds).not.toContain("ref: ${{ env.RELEASE_TAG }}");
  expect(builds).toContain("ref: ${{ needs.preflight.outputs.commit }}");
  expect(builds).toContain("releaseId: ${{ needs.prepare.outputs.release_id }}");
});

it("guards draft identity before uploads and verifies the notes round trip", () => {
  expect(workflow).toContain("node scripts/release-state.mjs prepare");
  expect(stateScript).toContain("releases?per_page=100");
  expect(workflow).toContain("node scripts/release-state.mjs check");
  expect(workflow).toContain("node scripts/release-state.mjs finalize");
});

it("uploads into the prepared release id without retargeting the tag", () => {
  const action = workflow.slice(workflow.indexOf("tauri-apps/tauri-action"), workflow.indexOf("\n  verify:"));
  expect(action).toContain("releaseId: ${{ needs.prepare.outputs.release_id }}");
  expect(action).not.toMatch(/^\s+tagName:/m);
  expect(action).not.toMatch(/^\s+releaseCommitish:/m);
});


it("supports hosted runners without an external devmap CLI and reports that absence", () => {
  const line = workflow.split("\n").find(line => line.includes("run: npm run check:vendor-schema"));
  expect(line).toBeDefined();
  const extra = line?.split(" -- ")[1]?.trim().split(/\s+/) ?? [];
  const result = spawnSync(process.execPath, [fileURLToPath(new URL("./check-vendor-schema.mjs", import.meta.url)), ...extra], {
    encoding: "utf8", env: {...process.env, PATH: "", Path: ""}, timeout: 5_000,
  });
  expect(result.status, result.stdout + result.stderr).toBe(0);
  expect(result.stdout).toContain("CLI unavailable");
});

it("fails the release when any platform leg fails rather than publishing a partial draft", () => {
  const verify = workflow.slice(workflow.indexOf("\n  verify:"));
  expect(verify).toContain("if: always()");
  expect(verify).toContain("needs.release.result");
  expect(verify).toContain("do not publish it");
});
