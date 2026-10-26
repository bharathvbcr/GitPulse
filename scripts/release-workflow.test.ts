import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { BUNDLE_CHECK_BUDGET_MS, DOWNLOAD_TIMEOUT_MS } from "./release-state.mjs";

const stateScript = readFileSync(new URL("./release-state.mjs", import.meta.url), "utf8");
const workflow = readFileSync(new URL("../.github/workflows/release.yml", import.meta.url), "utf8");

describe("release workflow contracts", () => {
  it("runs every repository contract gate before building release assets", () => {
    const preflight = workflow.slice(workflow.indexOf("preflight:"), workflow.indexOf("\n  release:"));
    expect(preflight).toContain("run: bun run check:ipc");
    expect(preflight).toContain("run: bun run check:types");
    expect(preflight).toContain("run: bun run check:release");
  });

  it("uses exact asset-manifest verification after the matrix completes", () => {
    expect(stateScript).toContain("inspectReleaseAssets({ tag, json: release })");
    expect(workflow).not.toContain("grep -qE");
  });

  it("takes the release body from the changelog, not a literal block", () => {
    expect(workflow).toContain("bun run release:notes -- --tag");
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
    expect(preflight).toContain("run: bun run release:notes -- --tag \"$RELEASE_TAG\"");
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

it("runs the macOS universal build through the package tauri script so PATH shims are reached", () => {
  // tauri-action finds bun.lock and runs `bun tauri`, which resolves to the
  // package.json tauri script, only when the action does not set tauriScript. That script is scripts/tauri.mjs,
  // which prepends scripts/bin so lipo and codesign wrap the system tools.
  // Override either and the next universal release dies at bundling/signing.
  expect(workflow).not.toMatch(/^\s*tauriScript:/m);
  const pkg = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));
  expect(pkg.scripts.tauri).toBe("node scripts/tauri.mjs");
});


it("supports hosted runners without an external devmap CLI and reports that absence", () => {
  const line = workflow.split("\n").find(line => line.includes("run: bun run check:vendor-schema"));
  expect(line).toBeDefined();
  const extra = line?.split(" -- ")[1]?.trim().split(/\s+/) ?? [];
  const result = spawnSync(process.execPath, [fileURLToPath(new URL("./check-vendor-schema.mjs", import.meta.url)), ...extra], {
    encoding: "utf8", env: {...process.env, PATH: "", Path: ""}, timeout: 5_000,
  });
  expect(result.status, result.stdout + result.stderr).toBe(0);
  expect(result.stdout).toContain("CLI unavailable");
});

it("pins third-party actions to the commit their tag resolved to", () => {
  const uses = [...workflow.matchAll(/^\s*uses:\s+(\S+)/gm)].map((match) => match[1]);
  const floating = uses.filter((use) => !/@[0-9a-f]{40}$/.test(use));
  expect(floating).toEqual([]);
  for (const pin of [
    // The stable branch's head. That branch's action.yml defaults `toolchain`
    // to stable, so the pin freezes the action's code and keeps its behaviour.
    "dtolnay/rust-toolchain@b725eda525362e7cf34af9a7243cac1e3f01a7ec # stable",
    "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7",
    "actions/setup-node@949feb2413d6458794dcd2491c4babbbce0c15c1 # v7",
    "oven-sh/setup-bun@0c5077e51419868618aeaa5fe8019c62421857d6 # v2",
    "Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2",
    "tauri-apps/tauri-action@1deb371b0cd8bd54025b384f1cd735e725c4060f # v1",
    "taiki-e/install-action@8cca0918f8471c33515202b73f56ab27f2bed283 # v2",
    "anchore/sbom-action@e22c389904149dbc22b58101806040fa8d37a610 # v0",
  ]) {
    expect(workflow).toContain(`uses: ${pin}`);
  }
});

it("does not persist credentials from any checkout", () => {
  const steps = workflow.split(/\n(?= {6}- )/);
  const checkouts = steps.filter((step) => step.includes("uses: actions/checkout@"));
  expect(checkouts).toHaveLength(5);
  for (const step of checkouts) {
    expect(step).toContain("persist-credentials: false");
  }
});

it("does not run dependency scripts while the release job holds the GitHub token", () => {
  const release = workflow.slice(workflow.indexOf("\n  release:"), workflow.indexOf("\n  attest:"));
  expect(release).toContain("contents: write");
  expect(release).toContain("run: bun install --frozen-lockfile --ignore-scripts");
  expect(release).not.toContain("run: bun install --frozen-lockfile\n");
});

it("fails the release when any platform leg fails rather than publishing a partial draft", () => {
  const verify = workflow.slice(workflow.indexOf("\n  verify:"));
  expect(verify).toContain("if: always()");
  expect(verify).toContain("needs.release.result");
  expect(verify).toContain("do not publish it");
});

it("builds Linux aarch64 and Windows arm64 on native runners with the Linux dependencies installed", () => {
  const release = workflow.slice(workflow.indexOf("\n  release:"), workflow.indexOf("\n  attest:"));
  for (const platform of ["macos-latest", "ubuntu-22.04", "ubuntu-22.04-arm", "windows-latest", "windows-11-arm"]) {
    expect(release).toContain(`- platform: '${platform}'`);
  }
  expect(release).toContain("if: startsWith(matrix.platform, 'ubuntu-')");
});

it("opens the bundled installers in verify, with the tools the runner image already has", () => {
  const verify = workflow.slice(workflow.indexOf("\n  verify:"));
  const finalize = verify.indexOf("node scripts/release-state.mjs finalize");
  expect(finalize).toBeGreaterThan(0);
  // finalize refuses to run without a download directory, so it must be set
  // in the same step; and ten minutes was sized before seven downloads.
  const step = verify.slice(verify.lastIndexOf("      - name:", finalize), finalize);
  expect(step).toContain("RELEASE_DOWNLOAD_DIR: ${{ runner.temp }}/bundles");
  expect(verify).toContain("timeout-minutes: 25");
  expect(verify).not.toContain("timeout-minutes: 10");
  // 7-Zip opens MSI and NSIS; it ships on the image (p7zip-full pulls in
  // 7zip), so nothing is installed and a missing tool fails by name.
  expect(verify).not.toMatch(/apt-get|apt install|msitools/);
  // One `command -v` per tool: `command -v a b` exits 0 when only one is found.
  expect(verify.slice(0, finalize)).toMatch(/command -v 7z\n\s+command -v gh\n/);
  expect(verify).toContain("The dmg, both rpms and both AppImages are not opened.");
  // The names it requires come from Cargo.toml, not from this workflow.
  expect(verify).not.toContain("gitpulse-hook");
  expect(stateScript).toContain("declaredBinaryNames(readFileSync(path.join(\"src-tauri\", \"Cargo.toml\")");
  expect(stateScript).toContain("downloadDir: process.env.RELEASE_DOWNLOAD_DIR");
});

it("sizes the bundle-check budget so it and the rest of the verify job fit inside timeout-minutes", () => {
  const verify = workflow.slice(workflow.indexOf("\n  verify:"));
  const limit = Number(/timeout-minutes:\s*(\d+)/.exec(verify)?.[1]);
  expect(limit).toBeGreaterThan(0);
  // release-state.mjs documents the arithmetic: the budget, 3 min for checkout,
  // setup-node and the tool check, and 8 min for the eight 60 s-bounded API and
  // git calls finalize makes outside the check.
  const budgetMinutes = BUNDLE_CHECK_BUDGET_MS / 60_000;
  const aroundTheCheck = 3 + 8;
  expect(budgetMinutes + aroundTheCheck).toBeLessThan(limit);
  expect(limit - (budgetMinutes + aroundTheCheck)).toBeGreaterThanOrEqual(2);
  // Per call, 120 s caps stay: the budget is the overall bound, the cap is per call.
  expect(DOWNLOAD_TIMEOUT_MS).toBe(120_000);
  expect(stateScript).toContain("12 min + 3 min + 8 min = 23 min");
});

it("tells whoever rolls a release back how to delete its tag, and what the pre-push hook does about it", () => {
  const contributing = readFileSync(new URL("../CONTRIBUTING.md", import.meta.url), "utf8");
  const section = contributing.slice(contributing.indexOf("### Releasing and rolling back"));
  expect(section).toContain("git push origin :refs/tags/vX.Y.Z");
  expect(section).toContain("git tag -d vX.Y.Z");
  expect(section).toContain(".githooks/pre-push");
  expect(section).toMatch(/does not object to this/);
  expect(section).toMatch(/already\s+exists/);
  expect(section).toMatch(/\*\*unverified:\*\*[\s\S]*immutable releases[\s\S]*tag ruleset/);
  // The claim is about the hook's code: it skips a deletion and never reads the remote tag.
  const hook = readFileSync(new URL("../.githooks/pre-push", import.meta.url), "utf8");
  expect(hook).toContain('[ "$local_sha" = "$zero" ] && continue');
  expect(hook).not.toMatch(/ls-remote|fetch|refs\/tags\/[^\n]*\$remote_sha/);
});

it("attaches checksums and SBOMs after every installer and before finalize", () => {
  const attest = workflow.slice(workflow.indexOf("\n  attest:"), workflow.indexOf("\n  verify:"));
  expect(attest).toContain("needs: [preflight, prepare, release]");
  expect(attest).toContain("ref: ${{ needs.preflight.outputs.commit }}");
  expect(attest).toContain(
    "uses: anchore/sbom-action@e22c389904149dbc22b58101806040fa8d37a610 # v0",
  );
  expect(attest).toContain("upload-release-assets: false");
  expect(attest).toContain("cargo cyclonedx --manifest-path src-tauri/Cargo.toml");
  expect(attest).toContain("run: node scripts/release-state.mjs attest");
  const verify = workflow.slice(workflow.indexOf("\n  verify:"));
  expect(verify).toContain("needs: [preflight, prepare, release, attest]");
  expect(verify).toContain('if [ "${{ needs.attest.result }}" != "success" ]; then');
});
