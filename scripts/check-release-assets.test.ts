import { readFileSync } from "node:fs";
import { execFile } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { afterAll, describe, expect, it } from "vitest";
import { PLATFORM_INSTALLERS, expectedAssetNames, expectedInstallerNames, inspectBundles, inspectReleaseAssets, supplementNames } from "./check-release-assets.mjs";

const execFileAsync = promisify(execFile);
const scriptPath = fileURLToPath(new URL("./check-release-assets.mjs", import.meta.url));
const tempDirs: string[] = [];

afterAll(async () => {
  while (tempDirs.length > 0) {
    const dir = tempDirs.pop();
    if (dir) await rm(dir, { recursive: true, force: true });
  }
});

function payload(version = "0.0.3", names = expectedAssetNames(version), size = 10) {
  return JSON.stringify({
    assets: names.map((name) => ({ name, size, state: "uploaded" })),
  });
}

async function runScript(tag: string, source: string) {
  const dir = await mkdtemp(path.join(tmpdir(), "gitpulse-release-assets-"));
  tempDirs.push(dir);
  const jsonPath = path.join(dir, "assets.json");
  await writeFile(jsonPath, source);
  try {
    const { stdout, stderr } = await execFileAsync(process.execPath, [
      scriptPath,
      "--tag",
      tag,
      "--json",
      jsonPath,
    ]);
    return { code: 0, output: stdout + stderr };
  } catch (err) {
    const failure = err as { code?: number | null; stdout?: string; stderr?: string };
    return {
      code: failure.code ?? 1,
      output: (failure.stdout ?? "") + (failure.stderr ?? ""),
    };
  }
}

describe("release asset completeness contract", () => {
  it("accepts the exact non-empty asset manifest for the configured matrix", async () => {
    const result = inspectReleaseAssets({ tag: "v0.0.3", json: payload() });
    expect(result.ok).toBe(true);
    expect(result.violations).toEqual([]);

    const cli = await runScript("v0.0.3", payload());
    expect(cli.code).toBe(0);
    expect(cli.output).toContain("OK: release asset manifest holds");
  });

  it("rejects stale, wrong-version, missing, and unexpected assets", async () => {
    const names = expectedAssetNames("0.0.3").filter((name) => !name.endsWith(".dmg"));
    names.push("GitPulse_0.0.2_universal.dmg", "old-release.deb");
    const result = inspectReleaseAssets({
      tag: "v0.0.3",
      json: payload("0.0.3", names),
    });
    expect(result.ok).toBe(false);
    expect(result.violations.join("\n")).toMatch(/missing: GitPulse_0\.0\.3_universal\.dmg/);
    expect(result.violations.join("\n")).toMatch(/unexpected: GitPulse_0\.0\.2_universal\.dmg/);
    expect(result.violations.join("\n")).toMatch(/unexpected: old-release\.deb/);

    const cli = await runScript("v0.0.3", payload("0.0.3", names));
    expect(cli.code).toBe(1);
  });

  it("treats duplicate names, zero-size assets, malformed JSON, and bad tags as invalid input", async () => {
    const names = expectedAssetNames("0.0.3");
    const duplicate = JSON.stringify({
      assets: [
        ...names.map((name) => ({ name, size: 10, state: "uploaded" })),
        { name: names[0], size: 10, state: "uploaded" },
      ],
    });
    expect(inspectReleaseAssets({ tag: "v0.0.3", json: duplicate }).invalid).toBe(true);
    expect(
      inspectReleaseAssets({
        tag: "v0.0.3",
        json: payload("0.0.3", names, 0),
      }).invalid,
    ).toBe(true);

    expect(inspectReleaseAssets({ tag: "v0.0.3", json: "not json" }).invalid).toBe(true);
    const cli = await runScript("release-0.0.3", payload());
    expect(cli.code).toBe(2);
  });

  it("accepts the real payload gh produced for v0.0.2", () => {
    // scripts/fixtures/gh-release-view-v1.json is `gh release view --json
    // assets` for a real release — the exact command release.yml runs — with
    // per-account noise (urls, ids, download counts) dropped and every other
    // field kept verbatim.
    //
    // This checker runs only on a `v*` tag, so a wrong expectation here would
    // surface at release time after the whole matrix had built. That is exactly
    // what happened when `tauri-apps/tauri-action` went v0 -> v1 and started
    // versioning the macOS updater archive.
    const payload = readFileSync(
      new URL("./fixtures/gh-release-view-v1.json", import.meta.url),
      "utf8",
    );
    //
    // That release predates the ARM legs and the attest stage, so its seven
    // names must all still be expected, and what it lacks must be exactly the
    // five ARM installers and three supplements added since — no renamed
    // x64 or universal asset may hide in either list.
    const result = inspectReleaseAssets({ tag: "v0.0.3", json: payload });
    expect(result.invalid).toBe(false);
    expect(result.violations.filter((violation) => !violation.startsWith("missing: "))).toEqual([]);
    const supplements = Object.values(supplementNames("0.0.3"));
    const arm = expectedInstallerNames("0.0.3").filter((name) => /aarch64|arm64/.test(name));
    expect(arm).toHaveLength(5);
    expect(result.violations.map((violation) => violation.slice("missing: ".length)).sort())
      .toEqual([...arm, ...supplements].sort());
    expect(result.actual).toEqual(expectedInstallerNames("0.0.3").filter((name) => !arm.includes(name)));
  });

  it("rejects the pre-v1 archive name, so the rename cannot silently come back", () => {
    // The v0.0.2 payload is kept verbatim: under `tauri-action@v0` the macOS
    // updater archive carried no version. It is a real payload that this
    // manifest must now refuse, which is what proves the manifest tracks the
    // action's naming rather than accepting whatever shows up.
    const payload = readFileSync(
      new URL("./fixtures/gh-release-view.json", import.meta.url),
      "utf8",
    );
    const result = inspectReleaseAssets({ tag: "v0.0.2", json: payload });
    expect(result.ok).toBe(false);
    expect(result.violations).toContain("missing: GitPulse_0.0.2_universal.app.tar.gz");
    expect(result.violations).toContain("unexpected: GitPulse_universal.app.tar.gz");
  });

  it("notices if a platform silently stops producing an installer", () => {
    // The failure this exists to catch: a green matrix that uploaded less
    // than it should have.
    const payload = JSON.parse(
      readFileSync(new URL("./fixtures/gh-release-view.json", import.meta.url), "utf8"),
    ) as { assets: Array<{ name: string }> };
    payload.assets = payload.assets.filter((asset) => !asset.name.endsWith(".msi"));
    const result = inspectReleaseAssets({ tag: "v0.0.2", json: JSON.stringify(payload) });
    expect(result.ok).toBe(false);
    expect(result.violations.join(" ")).toContain(".msi");
  });
});


describe("release metadata cannot imply an unperformed upload", () => {
  it.each([undefined, null, "new", "starter", "failed"])("rejects upload state %s", (state) => {
    const assets = expectedAssetNames("1.2.3").map(name => ({ name, size: 10, state }));
    expect(inspectReleaseAssets({tag: "v1.2.3", json: {assets}}).invalid).toBe(true);
  });
});

describe("installer manifest follows the release matrix", () => {
  it("names exactly the runners release.yml builds on", () => {
    const workflow = readFileSync(new URL("../.github/workflows/release.yml", import.meta.url), "utf8");
    const matrix = workflow.slice(workflow.indexOf("\n  release:"), workflow.indexOf("\n  attest:"));
    const platforms = [...matrix.matchAll(/- platform: '([^']+)'/g)].map((match) => match[1]).sort();
    expect(Object.keys(PLATFORM_INSTALLERS).sort()).toEqual(platforms);
  });

  it("names exactly the ARM runners ci.yml's arm-bundle job proves", () => {
    const ci = readFileSync(new URL("../.github/workflows/ci.yml", import.meta.url), "utf8");
    const job = ci.slice(ci.indexOf("\n  arm-bundle:"), ci.indexOf("\n  gusset:"));
    const legs = /platform: \[([^\]]+)\]/.exec(job)?.[1].split(",").map((leg) => leg.trim()).sort();
    const arm = Object.keys(PLATFORM_INSTALLERS).filter((platform) => platform.endsWith("-arm")).sort();
    expect(arm).toHaveLength(2);
    expect(legs).toEqual(arm);
    expect(job).toContain("node scripts/check-release-assets.mjs --bundles");
  });
});

describe("local bundle names", () => {
  const linuxArm = [
    "/w/src-tauri/target/release/bundle/deb/GitPulse_1.4.0_arm64.deb",
    "/w/src-tauri/target/release/bundle/rpm/GitPulse-1.4.0-1.aarch64.rpm",
    "/w/src-tauri/target/release/bundle/appimage/GitPulse_1.4.0_aarch64.AppImage",
  ];
  const windowsArm = [
    "D:\\a\\GitPulse\\src-tauri\\target\\release\\bundle\\msi\\GitPulse_1.4.0_arm64_en-US.msi",
    "D:\\a\\GitPulse\\src-tauri\\target\\release\\bundle\\nsis\\GitPulse_1.4.0_arm64-setup.exe",
  ];

  it("accepts each ARM runner's bundler output, Windows separators included", () => {
    expect(inspectBundles({ platform: "ubuntu-22.04-arm", version: "1.4.0", paths: linuxArm }).ok).toBe(true);
    expect(inspectBundles({ platform: "windows-11-arm", version: "1.4.0", paths: windowsArm }).ok).toBe(true);
  });

  it("rejects an x64 installer produced on an ARM runner, and a missing format", () => {
    const result = inspectBundles({
      platform: "ubuntu-22.04-arm",
      version: "1.4.0",
      paths: [linuxArm[0], linuxArm[1], "/w/bundle/appimage/GitPulse_1.4.0_amd64.AppImage"],
    });
    expect(result.ok).toBe(false);
    expect(result.violations).toEqual([
      "missing: GitPulse_1.4.0_aarch64.AppImage",
      "unexpected: GitPulse_1.4.0_amd64.AppImage",
    ]);
  });

  it("refuses what it cannot judge rather than passing it", () => {
    expect(() => inspectBundles({ platform: "macos-latest", version: "1.4.0", paths: ["/x/GitPulse.app.tar.gz"] })).toThrow(/rewritten at upload/);
    expect(() => inspectBundles({ platform: "solaris", version: "1.4.0", paths: linuxArm })).toThrow(/unknown release platform/);
    expect(() => inspectBundles({ platform: "windows-11-arm", version: "1.4.0", paths: [] })).toThrow(/non-empty array/);
  });

  it("checks against tauri.conf.json's version from the CLI", async () => {
    const config = JSON.parse(readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8")) as { version: string };
    const paths = PLATFORM_INSTALLERS["windows-11-arm"](config.version).map((name) => `C:\\b\\${name}`);
    const ok = await execFileAsync(process.execPath, [scriptPath, "--bundles", JSON.stringify(paths), "--platform", "windows-11-arm"]);
    expect(ok.stdout).toContain("OK: windows-11-arm bundled exactly");
    await expect(execFileAsync(process.execPath, [scriptPath, "--bundles", JSON.stringify(paths.slice(1)), "--platform", "windows-11-arm"]))
      .rejects.toMatchObject({ code: 1 });
    await expect(execFileAsync(process.execPath, [scriptPath, "--bundles", "[]", "--platform", "macos-latest", "--tag", "v1.0.0"]))
      .rejects.toMatchObject({ code: 2 });
  });
});
