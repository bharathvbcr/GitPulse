import { cpSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { expect, it } from "vitest";

function fixture() {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), "gitpulse-vendor-regression-")));
  const upstream = path.join(root, "upstream");
  for (const dir of ["scripts", "src-tauri/vendored", "upstream/rust-port/crates/dc-glob/src"]) mkdirSync(path.join(root, dir), { recursive: true });
  for (const file of ["vendor-crates.mjs", "usage.mjs", "columns.mjs"]) cpSync(new URL(file, import.meta.url), path.join(root, "scripts", file));
  writeFileSync(path.join(upstream, "rust-port/Cargo.toml"), '[workspace]\n[workspace.package]\nversion = "1.0.0"\n');
  writeFileSync(path.join(upstream, "rust-port/crates/dc-glob/Cargo.toml"), '[package]\nname = "dc-glob"\nversion.workspace = true\n');
  writeFileSync(path.join(upstream, "rust-port/crates/dc-glob/src/lib.rs"), "pub const VALUE: u8 = 7;\n");
  const manifest = path.join(root, "src-tauri/vendored/VENDOR.json");
  writeFileSync(manifest, '{"crates":[]}');
  const run = (...args: string[]) => spawnSync(process.execPath, [path.join(root, "scripts/vendor-crates.mjs"), ...args, "--json"], {
    encoding: "utf8", timeout: 15_000,
    env: { ...process.env, GITPULSE_ALLOW_DRIFT: "0", GITPULSE_DEVCOUNCIL_ROOT: upstream, GITPULSE_MARKDEV_ROOT: "/missing" },
  });
  const initial = run("--crate=dc-glob");
  expect(initial.status, initial.stderr).toBe(0);
  return { root, upstream, manifest, run, cleanup: () => rmSync(root, { recursive: true, force: true }) };
}

it("detects upstream deletions and inherited manifest changes", () => {
  const f = fixture();
  try {
    rmSync(path.join(f.upstream, "rust-port/crates/dc-glob/src/lib.rs"));
    writeFileSync(path.join(f.upstream, "rust-port/Cargo.toml"), '[workspace]\n[workspace.package]\nversion = "2.0.0"\n');
    const result = f.run("--check");
    expect(result.status, result.stderr).toBe(1);
    const report = JSON.parse(result.stdout);
    expect(report.crates[0].upstream).toBe("drifted");
    expect(report.crates[0].drifted).toEqual(expect.arrayContaining(["Cargo.toml", "src/lib.rs"]));
    expect(report.crates[0].edited).toEqual([]);
  } finally { f.cleanup(); }
});

it.each(["edited", "missing", "extra"])("checks locally maintained framework snapshots for %s files", (mode) => {
  const f = fixture();
  try {
    const dir = path.join(f.root, "src-tauri/framework/gtk-consumer");
    mkdirSync(dir, { recursive: true });
    const original = "pub const VERSION: u8 = 1;\n";
    writeFileSync(path.join(dir, "lib.rs"), original);
    writeFileSync(path.join(f.root, "src-tauri/framework/PATCHES.json"), JSON.stringify({ crates: [{
      name: "gtk-consumer", origin: { repo: "https://example.com/upstream", commit: "pinned" },
      files: { "lib.rs": createHash("sha256").update(original).digest("hex") },
    }] }));
    expect(f.run("--check", "--allow-drift").status).toBe(0);
    if (mode === "edited") writeFileSync(path.join(dir, "lib.rs"), "changed\n");
    if (mode === "missing") rmSync(path.join(dir, "lib.rs"));
    if (mode === "extra") writeFileSync(path.join(dir, "extra.rs"), "unrecorded\n");
    const result = f.run("--check", "--allow-drift");
    expect(result.status, result.stderr).toBe(1);
    expect(JSON.parse(result.stdout).crates).toContainEqual(expect.objectContaining({
      name: "gtk-consumer", upstream: "unavailable", edited: expect.arrayContaining([expect.stringMatching(/\.rs/)]),
    }));
  } finally { f.cleanup(); }
});

it("refuses a missing framework tree required by the application manifest", () => {
  const f = fixture();
  try {
    writeFileSync(path.join(f.root, "src-tauri/Cargo.toml"), '[dependencies]\ntauri = { path = "framework/tauri" }\n');
    const result = f.run("--check", "--allow-drift");
    expect(result.status).toBe(2);
    expect(result.stderr).toContain("PATCHES.json");
  } finally { f.cleanup(); }
});

it.each(["--crate=dc-glob", "full"])("preserves every old byte when %s preparation fails", (mode) => {
  const f = fixture();
  try {
    const oldManifest = readFileSync(f.manifest, "utf8");
    const library = path.join(f.root, "src-tauri/vendored/dc-glob/src/lib.rs");
    const oldLibrary = readFileSync(library, "utf8");
    writeFileSync(path.join(f.upstream, "rust-port/crates/dc-glob/src/lib.rs"), "changed bytes\n");
    if (mode !== "full") writeFileSync(path.join(f.upstream, "rust-port/crates/dc-glob/Cargo.toml"), '[package]\nname = "dc-glob"\nmissing.workspace = true\n');
    const result = mode === "full" ? f.run() : f.run(mode);
    expect(result.status).toBe(2);
    expect(readFileSync(f.manifest, "utf8")).toBe(oldManifest);
    expect(readFileSync(library, "utf8")).toBe(oldLibrary);
  } finally { f.cleanup(); }
});

it("refuses a concurrent refresh and leaves the old snapshot intact", () => {
  const f = fixture();
  try {
    const oldManifest = readFileSync(f.manifest, "utf8");
    mkdirSync(path.join(f.root, "src-tauri/.vendor-lock"));
    const result = f.run("--crate=dc-glob");
    expect(result.status).toBe(2);
    expect(result.stderr).toContain("vendor-lock");
    expect(readFileSync(f.manifest, "utf8")).toBe(oldManifest);
  } finally { f.cleanup(); }
});

it.runIf(process.platform !== "win32")("rejects source symlinks instead of copying content outside the module", () => {
  const f = fixture();
  try {
    writeFileSync(path.join(f.root, "outside.rs"), "external bytes");
    symlinkSync(path.join(f.root, "outside.rs"), path.join(f.upstream, "rust-port/crates/dc-glob/src/escape.rs"));
    const before = readFileSync(f.manifest, "utf8");
    const result = f.run("--crate=dc-glob");
    expect(result.status).toBe(2);
    expect(result.stderr).toMatch(/symlink|symbolic/i);
    expect(readFileSync(f.manifest, "utf8")).toBe(before);
  } finally { f.cleanup(); }
});

it("keeps repeated source, manifest, and deletion updates coherent", () => {
  const f = fixture();
  try {
    for (let generation = 0; generation < 12; generation++) {
      const source = path.join(f.upstream, "rust-port/crates/dc-glob");
      const optional = path.join(source, "build.rs");
      if (generation % 2 === 0) writeFileSync(optional, `fn main() { println!("generation ${generation}"); }`);
      else rmSync(optional);
      writeFileSync(path.join(source, "src/lib.rs"), `pub const GENERATION: usize = ${generation};\n`);
      writeFileSync(path.join(f.upstream, "rust-port/Cargo.toml"), `[workspace]\n[workspace.package]\nversion = "1.0.${generation}"\n`);
      expect(f.run("--check").status).toBe(1);
      const update = f.run("--crate=dc-glob");
      expect(update.status, update.stderr).toBe(0);
      const check = f.run("--check");
      expect(check.status, check.stderr).toBe(0);
      expect(JSON.parse(check.stdout).crates[0]).toMatchObject({ edited: [], upstream: "matches", drifted: [] });
    }
    const before = readFileSync(f.manifest, "utf8");
    expect(f.run("--crate=dc-glob").status).toBe(0);
    expect(readFileSync(f.manifest, "utf8")).toBe(before);
  } finally { f.cleanup(); }
}, 30_000);

it("excludes local state and upstream tests from both snapshot and comparison", () => {
  const f = fixture();
  try {
    const source = path.join(f.upstream, "rust-port/crates/dc-glob");
    mkdirSync(path.join(source, "src/.devcouncil"));
    mkdirSync(path.join(source, "tests"));
    writeFileSync(path.join(source, "src/.devcouncil/session"), "local state");
    writeFileSync(path.join(source, "tests/upstream.rs"), "test-only");
    expect(f.run("--check").status).toBe(0);
    expect(f.run("--crate=dc-glob").status).toBe(0);
    expect(readFileSync(f.manifest, "utf8")).not.toContain("session");
    expect(readFileSync(f.manifest, "utf8")).not.toContain("upstream.rs");
  } finally { f.cleanup(); }
});

it("updates one crate without reading or rewriting unrelated upstreams", () => {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), "gitpulse-vendor-scope-")));
  try {
    for (const dir of ["scripts", "src-tauri/vendored/untouched", "upstream/rust-port/crates/dc-glob/src"]) mkdirSync(path.join(root, dir), { recursive: true });
    for (const file of ["vendor-crates.mjs", "usage.mjs", "columns.mjs"]) cpSync(new URL(file, import.meta.url), path.join(root, "scripts", file));
    writeFileSync(path.join(root, "upstream/rust-port/Cargo.toml"), "[workspace]\n[workspace.package]\nversion = \"1.0.0\"\n");
    writeFileSync(path.join(root, "upstream/rust-port/crates/dc-glob/Cargo.toml"), "[package]\nname = \"dc-glob\"\nversion.workspace = true\n");
    writeFileSync(path.join(root, "upstream/rust-port/crates/dc-glob/src/lib.rs"), "pub const VALUE: u8 = 7;\n");
    const untouched = { name: "untouched", origin: { commit: "preserved" }, files: { "lib.rs": "preserved" } };
    writeFileSync(path.join(root, "src-tauri/vendored/untouched/lib.rs"), "original bytes");
    const manifestPath = path.join(root, "src-tauri/vendored/VENDOR.json");
    writeFileSync(manifestPath, JSON.stringify({ crates: [untouched] }));
    const run = (crate: string) => spawnSync(process.execPath, [path.join(root, "scripts/vendor-crates.mjs"), `--crate=${crate}`, "--json"], {
      encoding: "utf8", env: { ...process.env, GITPULSE_DEVCOUNCIL_ROOT: path.join(root, "upstream"), GITPULSE_MARKDEV_ROOT: "/missing" },
    });
    const result = run("dc-glob");
    expect(result.status, result.stderr).toBe(0);
    expect(readFileSync(path.join(root, "src-tauri/vendored/untouched/lib.rs"), "utf8")).toBe("original bytes");
    const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    expect(manifest.crates.find((c: {name: string}) => c.name === "untouched")).toEqual(untouched);
    expect(readFileSync(path.join(root, "src-tauri/vendored/dc-glob/src/lib.rs"), "utf8")).toContain("VALUE: u8 = 7");
    const before = readFileSync(manifestPath, "utf8");
    expect(run("../escape").status).toBe(2);
    expect(readFileSync(manifestPath, "utf8")).toBe(before);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
