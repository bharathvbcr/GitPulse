import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { createHash } from "node:crypto";
import { writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { build, type Plugin } from "vite";
import { describe, expect, it } from "vitest";
import { privateSourceMaps, pruneEvidence, shippedSourceMaps } from "./build-evidence.mjs";
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

  // Hermetic: a shallow or tag-less checkout of this repository (CI's default
  // clone, a fresh cloud session) has no tags to read, so the fixture makes
  // its own — one lightweight tag and one annotated tag on distinct commits.
  it("lists the commit a release tag points at, peeled through annotated tags", async () => {
    const root = await mkdtemp(path.join(tmpdir(), "gitpulse-tags-"));
    try {
      const git = (...args: string[]) =>
        execFileSync("git", ["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false", ...args], { cwd: root, encoding: "utf8" }).trim();
      git("init", "-q");
      git("commit", "-q", "--allow-empty", "-m", "one");
      git("tag", "v1.0.0");
      const light = git("rev-parse", "HEAD");
      git("commit", "-q", "--allow-empty", "-m", "two");
      git("tag", "-a", "v1.1.0", "-m", "release");
      const annotated = git("rev-parse", "HEAD");
      const revisions = releaseRevisions(root);
      expect(revisions).toContain(light);
      expect(revisions).toContain(annotated);
    } finally { await rm(root, { recursive: true, force: true }); }
  });
});

describe("source maps never ship in the frontend bundle", () => {
  it("names every file that is, or points at, a source map", async () => {
    const root = await mkdtemp(path.join(tmpdir(), "gitpulse-leak-"));
    try {
      await mkdir(path.join(root, "assets", "deep"), { recursive: true });
      await writeFile(path.join(root, "assets", "main.js"), "export {};\n//# sourceMappingURL=main.js.map\n");
      await writeFile(path.join(root, "assets", "main.js.map"), "{}");
      await writeFile(path.join(root, "assets", "deep", "style.css"), "a{}\n/*# sourceMappingURL=style.css.map */");
      await writeFile(path.join(root, "assets", "old.mjs"), "x()\n//@ sourceMappingURL=old.mjs.map");
      await writeFile(path.join(root, "assets", "deep", "orphan.css.map"), "{}");
      expect(shippedSourceMaps(root)).toEqual([
        "assets/deep/orphan.css.map", "assets/deep/style.css", "assets/main.js", "assets/main.js.map", "assets/old.mjs",
      ]);
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("passes a bundle that only mentions the word, and an empty one", async () => {
    const root = await mkdtemp(path.join(tmpdir(), "gitpulse-leak-"));
    try {
      expect(shippedSourceMaps(root)).toEqual([]);
      await writeFile(path.join(root, "vendor.js"), "const tag = \"sourceMappingURL=\"; export { tag };");
      await writeFile(path.join(root, "index.html"), "<!-- //# sourceMappingURL=x.map -->");
      await writeFile(path.join(root, "build-info.json"), "{}");
      expect(shippedSourceMaps(root)).toEqual([]);
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("fails the build, and prunes nothing, when a map reaches the output directory", async () => {
    const root = await mkdtemp(path.join(tmpdir(), "gitpulse-leak-"));
    try {
      const evidence = path.join(root, "evidence");
      await seedHistory(evidence);
      const before = (await readdir(evidence)).sort();
      const dist = path.join(root, "dist");
      // Runs before privateSourceMaps, so its map is written after that plugin
      // has already moved the bundler's own maps out of the way.
      const leak: Plugin = { name: "leak", writeBundle() { writeFileSync(path.join(dist, "leak.js.map"), "{}"); } };
      await expect(buildProbe(root, evidence, [leak])).rejects.toThrow(/would ship its source maps: leak\.js\.map/);
      // Retention is skipped: every snapshot that existed is still there. The
      // failed build's own snapshot was written before the check ran.
      const after = (await readdir(evidence)).sort();
      expect(after.filter((id) => before.includes(id))).toEqual(before);
      expect(after.length).toBe(before.length + 1);
    } finally { await rm(root, { recursive: true, force: true }); }
  });
});
