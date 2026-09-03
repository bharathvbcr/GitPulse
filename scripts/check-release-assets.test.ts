import { readFileSync } from "node:fs";
import { execFile } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { afterAll, describe, expect, it } from "vitest";
import { expectedAssetNames, inspectReleaseAssets } from "./check-release-assets.mjs";

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
    const result = inspectReleaseAssets({ tag: "v0.0.3", json: payload });
    expect(result.violations).toEqual([]);
    expect(result.invalid).toBe(false);
    expect(result.ok).toBe(true);
    expect(result.actual).toEqual(expectedAssetNames("0.0.3"));
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
