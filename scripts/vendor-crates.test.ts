import { execFileSync, spawnSync } from "node:child_process";
import {
  cpSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { describe, expect, it } from "vitest";

const SOURCE_TABLE = [
  { id: "manvi", workspace: "crates", crateBase: "crates", crates: ["dc-glob", "dc-store", "dc-verify"] },
  {
    id: "devcouncil",
    workspace: "rust-port",
    crateBase: "rust-port/crates",
    crates: ["devmap-analyze", "devmap-extract", "devmap-query", "devmap-resolve", "devmap-store"],
  },
] as const;

const MAX_MANIFEST_BYTES = 1024 * 1024;

function snapshotTree(root: string) {
  const files: Record<string, string> = {};
  const visit = (dir: string, prefix = "") => {
    for (const name of readdirSync(dir).sort()) {
      const full = path.join(dir, name);
      const relative = prefix ? `${prefix}/${name}` : name;
      const info = lstatSync(full);
      if (info.isDirectory()) visit(full, relative);
      else files[relative] = readFileSync(full).toString("base64");
    }
  };
  visit(root);
  return files;
}

function fixture() {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), "gitpulse-vendor-regression-")));
  const scripts = path.join(root, "scripts");
  const vendorDir = path.join(root, "src-tauri", "vendored");
  mkdirSync(scripts, { recursive: true });
  mkdirSync(vendorDir, { recursive: true });
  for (const file of ["vendor-crates.mjs", "usage.mjs", "columns.mjs"]) {
    cpSync(new URL(file, import.meta.url), path.join(scripts, file));
  }

  const roots: Record<string, string> = {};
  for (const source of SOURCE_TABLE) {
    const sourceRoot = path.join(root, source.id);
    roots[source.id] = sourceRoot;
    mkdirSync(path.join(sourceRoot, source.workspace), { recursive: true });
    writeFileSync(
      path.join(sourceRoot, source.workspace, "Cargo.toml"),
      '[workspace]\n[workspace.package]\nversion = "1.0.0"\n',
    );
    for (const crate of source.crates) {
      const crateDir = path.join(sourceRoot, source.crateBase, crate);
      mkdirSync(path.join(crateDir, "src"), { recursive: true });
      writeFileSync(path.join(crateDir, "Cargo.toml"), `[package]\nname = "${crate}"\nversion.workspace = true\n`);
      writeFileSync(path.join(crateDir, "src", "lib.rs"), `pub const NAME: &str = "${crate}";\n`);
    }
  }

  const markdev = path.join(root, "markdev");
  roots.markdev = markdev;
  mkdirSync(path.join(markdev, "core", "src"), { recursive: true });
  writeFileSync(path.join(markdev, "core", "Cargo.toml"), '[package]\nname = "markdev"\nversion = "1.0.0"\n');
  writeFileSync(path.join(markdev, "core", "src", "lib.rs"), 'pub const NAME: &str = "markdev";\n');

  const env = {
    ...process.env,
    GITPULSE_ALLOW_DRIFT: "0",
    GITPULSE_MANVI_ROOT: roots.manvi,
    GITPULSE_DEVCOUNCIL_ROOT: roots.devcouncil,
    GITPULSE_MARKDEV_ROOT: roots.markdev,
  };
  const run = (...args: string[]) =>
    spawnSync(process.execPath, [path.join(scripts, "vendor-crates.mjs"), ...args, "--json"], {
      encoding: "utf8",
      env,
      timeout: 15_000,
    });
  const initial = run();
  expect(initial.status, initial.stderr).toBe(0);
  return {
    root,
    roots,
    vendorDir,
    env,
    run,
    cleanup: () => rmSync(root, { recursive: true, force: true }),
  };
}

describe("vendor refresh integrity", () => {
  it("preserves the complete old snapshot when a late manifest fails", () => {
    const f = fixture();
    try {
      const manifestPath = path.join(f.vendorDir, "VENDOR.json");
      const libraryPath = path.join(f.vendorDir, "dc-glob", "src", "lib.rs");
      const oldManifest = readFileSync(manifestPath, "utf8");
      const oldLibrary = readFileSync(libraryPath, "utf8");
      writeFileSync(path.join(f.roots.manvi, "crates", "dc-glob", "src", "lib.rs"), "changed before failure\n");
      writeFileSync(
        path.join(f.roots.markdev, "core", "Cargo.toml"),
        '[package]\nname = "markdev"\nversion.workspace = true\n',
      );

      const result = f.run();
      expect(result.status).toBe(2);
      expect(readFileSync(manifestPath, "utf8")).toBe(oldManifest);
      expect(readFileSync(libraryPath, "utf8")).toBe(oldLibrary);
    } finally {
      f.cleanup();
    }
  });

  it("detects deleted upstream files and resolved Cargo manifest drift", () => {
    const f = fixture();
    try {
      const crateDir = path.join(f.roots.manvi, "crates", "dc-glob");
      const removed = path.join(crateDir, "src", "lib.rs");
      rmSync(removed);
      writeFileSync(
        path.join(f.roots.manvi, "crates", "Cargo.toml"),
        '[workspace]\n[workspace.package]\nversion = "2.0.0"\n',
      );

      const result = f.run("--check");
      expect(result.status, result.stderr).toBe(1);
      const crate = JSON.parse(result.stdout).crates.find((entry: { name: string }) => entry.name === "dc-glob");
      expect(crate.upstream).toBe("drifted");
      expect(crate.drifted).toEqual(expect.arrayContaining(["Cargo.toml", "src/lib.rs"]));
      expect(crate.edited).toEqual([]);
    } finally {
      f.cleanup();
    }
  });

  it("keeps local edits separate from upstream drift", () => {
    const f = fixture();
    try {
      writeFileSync(path.join(f.vendorDir, "dc-glob", "src", "lib.rs"), "edited only in GitPulse\n");
      const result = f.run("--check");
      expect(result.status, result.stderr).toBe(1);
      const crate = JSON.parse(result.stdout).crates.find((entry: { name: string }) => entry.name === "dc-glob");
      expect(crate.edited).toEqual(["src/lib.rs"]);
      expect(crate.upstream).toBe("matches");
      expect(crate.drifted).toEqual([]);
    } finally {
      f.cleanup();
    }
  });

  it("updates one crate without requiring or rewriting unrelated upstreams", () => {
    const f = fixture();
    try {
      const untouched = path.join(f.vendorDir, "markdev", "src", "lib.rs");
      const oldUntouched = readFileSync(untouched, "utf8");
      writeFileSync(
        path.join(f.roots.manvi, "crates", "dc-glob", "src", "lib.rs"),
        "pub const GENERATION: usize = 2;\n",
      );
      const result = spawnSync(
        process.execPath,
        [path.join(f.root, "scripts", "vendor-crates.mjs"), "--crate=dc-glob", "--json"],
        {
          encoding: "utf8",
          env: {
            ...process.env,
            GITPULSE_MANVI_ROOT: f.roots.manvi,
            GITPULSE_DEVCOUNCIL_ROOT: "/missing/devcouncil",
            GITPULSE_MARKDEV_ROOT: "/missing/markdev",
          },
        },
      );
      expect(result.status, result.stderr).toBe(0);
      expect(readFileSync(path.join(f.vendorDir, "dc-glob", "src", "lib.rs"), "utf8")).toContain("GENERATION");
      expect(readFileSync(untouched, "utf8")).toBe(oldUntouched);
    } finally {
      f.cleanup();
    }
  });

  it.runIf(process.platform !== "win32")("rejects source symlinks before replacing the live snapshot", () => {
    const f = fixture();
    try {
      const manifest = path.join(f.vendorDir, "VENDOR.json");
      const before = readFileSync(manifest, "utf8");
      const outside = path.join(f.root, "outside.rs");
      writeFileSync(outside, "external bytes\n");
      symlinkSync(outside, path.join(f.roots.manvi, "crates", "dc-glob", "src", "escape.rs"));
      const result = f.run();
      expect(result.status).toBe(2);
      expect(result.stderr).toMatch(/symbolic link|symlink/i);
      expect(readFileSync(manifest, "utf8")).toBe(before);
    } finally {
      f.cleanup();
    }
  });

  it("rejects a concurrent refresh without changing the recorded snapshot", () => {
    const f = fixture();
    try {
      const manifest = path.join(f.vendorDir, "VENDOR.json");
      const before = readFileSync(manifest, "utf8");
      mkdirSync(path.join(f.root, "src-tauri", ".vendor-lock"));
      const result = f.run();
      expect(result.status).toBe(2);
      expect(result.stderr).toContain("vendor-lock");
      expect(readFileSync(manifest, "utf8")).toBe(before);
    } finally {
      f.cleanup();
    }
  });

  for (const location of ["workspace", "crate"] as const) {
    for (const shape of ["symlink", "fifo", "oversized"] as const) {
      it.runIf(shape === "oversized" || process.platform !== "win32")(
        `refuses a ${shape} ${location} manifest in refresh and check modes without replacing the snapshot`,
        () => {
          const f = fixture();
          try {
            const beforeTree = snapshotTree(f.vendorDir);
            const cargo =
              location === "workspace"
                ? path.join(f.roots.manvi, "crates", "Cargo.toml")
                : path.join(f.roots.manvi, "crates", "dc-glob", "Cargo.toml");
            const original = readFileSync(cargo, "utf8");
            rmSync(cargo);
            if (shape === "symlink") {
              const target = path.join(f.root, `${location}-Cargo.toml`);
              writeFileSync(target, original);
              symlinkSync(target, cargo);
            } else if (shape === "fifo") {
              execFileSync("mkfifo", [cargo]);
            } else {
              writeFileSync(cargo, `${original}\n#${"x".repeat(MAX_MANIFEST_BYTES)}\n`);
            }

            for (const args of [[], ["--check"]]) {
              const result = spawnSync(
                process.execPath,
                [path.join(f.root, "scripts", "vendor-crates.mjs"), ...args, "--json"],
                { encoding: "utf8", env: f.env, timeout: 2_000 },
              );
              expect(result.error, `${location} ${shape} ${args.join(" ")}`).toBeUndefined();
              expect(result.status, result.stderr).toBe(2);
              expect(result.stderr).toMatch(/manifest|regular file|symbolic link|too large/i);
              expect(snapshotTree(f.vendorDir)).toEqual(beforeTree);
            }
          } finally {
            f.cleanup();
          }
        },
        10_000,
      );
    }
  }

  it("accepts a regular workspace manifest at the byte limit", () => {
    const f = fixture();
    try {
      const cargo = path.join(f.roots.manvi, "crates", "Cargo.toml");
      const prefix = `${readFileSync(cargo, "utf8")}\n#`;
      writeFileSync(cargo, `${prefix}${"x".repeat(MAX_MANIFEST_BYTES - Buffer.byteLength(prefix))}`);
      expect(lstatSync(cargo).size).toBe(MAX_MANIFEST_BYTES);
      const result = f.run();
      expect(result.status, result.stderr).toBe(0);
    } finally {
      f.cleanup();
    }
  });
});

describe("sibling discovery", () => {
  it.runIf(process.platform !== "win32")("uses the linked worktree's common Git directory", () => {
    const root = realpathSync(mkdtempSync(path.join(tmpdir(), "gitpulse-vendor-sibling-")));
    try {
      const canonical = path.join(root, "Code", "devtools", "GitPulse");
      const worktree = path.join(root, ".codex", "worktrees", "task", "GitPulse");
      mkdirSync(canonical, { recursive: true });
      for (const sibling of ["Manvi", "DevCouncil", "MarkDev"]) {
        mkdirSync(path.join(root, "Code", "devtools", sibling), { recursive: true });
      }
      execFileSync("git", ["init", "-q", canonical]);
      writeFileSync(path.join(canonical, "seed"), "seed\n");
      execFileSync("git", ["-C", canonical, "add", "seed"]);
      execFileSync("git", ["-C", canonical, "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "seed"]);
      mkdirSync(path.dirname(worktree), { recursive: true });
      execFileSync("git", ["-C", canonical, "worktree", "add", "-q", "--detach", worktree, "HEAD"]);

      const moduleUrl = pathToFileURL(path.join(import.meta.dirname, "vendor-crates.mjs")).href;
      const result = spawnSync(
        process.execPath,
        ["--input-type=module", "-e", `import { sources } from ${JSON.stringify(moduleUrl)}; console.log(JSON.stringify(sources({}, ${JSON.stringify(worktree)}).map(s => s.root)))`],
        { encoding: "utf8" },
      );
      expect(result.status, result.stderr).toBe(0);
      expect(JSON.parse(result.stdout)).toEqual(
        ["Manvi", "DevCouncil", "MarkDev"].map((name) => path.join(root, "Code", "devtools", name)),
      );
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});
