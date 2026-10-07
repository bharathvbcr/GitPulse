import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { build, type Plugin } from "vite";
import { describe, expect, it } from "vitest";
import { privateSourceMaps, pruneEvidence } from "./build-evidence.mjs";
import { appBuild, releaseRevisions } from "./app-version.mjs";

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

const HOUR = 60 * 60 * 1000;
const RELEASE = "a".repeat(40);

async function seed(evidence: string, id: string, manifest: Record<string, unknown> | null) {
  await mkdir(path.join(evidence, id), { recursive: true });
  if (manifest) await writeFile(path.join(evidence, id, "manifest.json"), JSON.stringify({ id, ...manifest }));
}

/** Seven dev builds, seven clean builds (the oldest a tagged release), and two
 * directories that are not provably snapshots. */
async function seedHistory(evidence: string) {
  const at = (hoursAgo: number) => new Date(Date.now() - hoursAgo * HOUR).toISOString();
  for (let i = 1; i <= 6; i++) await seed(evidence, `dirty-${i}`, { dirty: true, revision: "b".repeat(40), builtAt: at(i) });
  await seed(evidence, "dirty-stale", { dirty: true, revision: "b".repeat(40), builtAt: at(30 * 24) });
  for (let i = 1; i <= 6; i++) await seed(evidence, `clean-${i}`, { dirty: false, revision: `${i}`.repeat(40), builtAt: at(i) });
  await seed(evidence, "release", { dirty: false, revision: RELEASE, builtAt: at(400 * 24) });
  await seed(evidence, "no-manifest", null);
  await seed(evidence, "foreign", { id: "someone-else", dirty: true, revision: "c".repeat(40), builtAt: at(900) });
}

async function buildProbe(root: string, evidence: string, plugins: Plugin[] = []) {
  const entry = path.join(root, "probe.js");
  await writeFile(entry, "export const probe = 1;");
  const stamp = appBuild();
  await build({
    configFile: false, logLevel: "silent",
    plugins: [...plugins, privateSourceMaps(stamp, evidence, { releaseRevisions: () => new Set([RELEASE]) })],
    build: { outDir: path.join(root, "dist"), sourcemap: "hidden", lib: { entry, formats: ["es"], fileName: "probe" } },
  });
  return stamp;
}

describe("build evidence retention", () => {
  it("bounds dev snapshots after a successful build and never prunes a tagged release", async () => {
    const root = await mkdtemp(path.join(tmpdir(), "gitpulse-retain-"));
    try {
      const evidence = path.join(root, "evidence");
      await seedHistory(evidence);
      const stamp = await buildProbe(root, evidence);
      expect((await readdir(evidence)).sort()).toEqual([
        stamp.id,
        // Newest three dev builds; the rest and the month-old one are pruned.
        "dirty-1", "dirty-2", "dirty-3",
        // Five newest untagged clean builds; clean-6 is pruned.
        "clean-1", "clean-2", "clean-3", "clean-4", "clean-5",
        // The oldest snapshot of all, kept because a tag names its revision.
        "release",
        "no-manifest", "foreign",
      ].sort());
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("prunes nothing when the build fails", async () => {
    const root = await mkdtemp(path.join(tmpdir(), "gitpulse-retain-"));
    try {
      const evidence = path.join(root, "evidence");
      await seedHistory(evidence);
      const before = (await readdir(evidence)).sort();
      const broken: Plugin = { name: "broken", renderChunk() { throw new Error("render failed"); } };
      await expect(buildProbe(root, evidence, [broken])).rejects.toThrow("render failed");
      expect((await readdir(evidence)).sort()).toEqual(before);
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("keeps every clean snapshot when release tags cannot be listed", async () => {
    const root = await mkdtemp(path.join(tmpdir(), "gitpulse-retain-"));
    try {
      await seedHistory(root);
      const { pruned, failed } = pruneEvidence(root, "none", { releaseRevisions: () => null });
      expect(failed).toEqual([]);
      expect(pruned.sort()).toEqual(["dirty-4", "dirty-5", "dirty-6", "dirty-stale"]);
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("lists the commit a release tag points at, peeled through annotated tags", () => {
    const tagged = execFileSync("git", ["rev-list", "-n", "1", "--tags"], { encoding: "utf8" }).trim();
    expect(releaseRevisions()).toContain(tagged);
  });
});
