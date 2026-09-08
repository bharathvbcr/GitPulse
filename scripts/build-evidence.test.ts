import { mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { createHash } from "node:crypto";
import { build } from "vite";
import { describe, expect, it } from "vitest";
import { privateSourceMaps } from "./build-evidence.mjs";
import { appBuild } from "./app-version.mjs";

describe("exact build evidence", () => {
  it("distinguishes repeated builds from the same working tree", () => {
    expect(appBuild().id).not.toBe(appBuild().id);
  });
  it("retains matching source maps outside the distributable bundle", async () => {
    const root = await mkdtemp(path.join(tmpdir(), "gitpulse-maps-"));
    try {
      const entry = path.join(root, "probe.js");
      await writeFile(entry, 'export function crashProbe() { throw new Error("probe-error"); }');
      const stamp = appBuild();
      const evidenceRoot = path.join(root, "private");
      const outDir = path.join(root, "dist");
      await build({ configFile: false, logLevel: "silent", plugins: [privateSourceMaps(stamp, path.relative(process.cwd(), evidenceRoot))], build: { outDir, sourcemap: "hidden", lib: { entry, formats: ["es"], fileName: "probe" } } });
      const files = await readdir(outDir, { recursive: true });
      expect(files.some(file => file.endsWith(".map"))).toBe(false);
      const js = files.find(file => file.endsWith(".js"))!;
      const bundle = await readFile(path.join(outDir, js), "utf8");
      expect(bundle).not.toContain("sourceMappingURL");
      const evidence = path.join(evidenceRoot, stamp.id);
      const map = JSON.parse(await readFile(path.join(evidence, `${js}.map`), "utf8"));
      expect(map.sourcesContent.join("\n")).toContain("crashProbe");
      expect(map.mappings.length).toBeGreaterThan(0);
      const manifest = JSON.parse(await readFile(path.join(evidence, "manifest.json"), "utf8"));
      expect(manifest.id).toBe(stamp.id);
      expect(manifest.chunks[js]).toBe(createHash("sha256").update(bundle).digest("hex"));
      expect(JSON.parse(await readFile(path.join(outDir, "build-info.json"), "utf8")).id).toBe(stamp.id);
    } finally { await rm(root, { recursive: true, force: true }); }
  });
});
