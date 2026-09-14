import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { classifyProvenance } from "./check-mcp-install.mjs";
import {
  RECORD_VERSION,
  fileDigest,
  sourceDigest,
  sourceFiles,
} from "./install-identity.mjs";
import { installedBinNames } from "./record-install.mjs";

/**
 * The provenance half of the install doctor.
 *
 * It exists because version is a *release* identity: `gitpulse-hook` reports
 * `CARGO_PKG_VERSION` and nothing else, so a binary built before a fix reports
 * a version that still matches and the doctor called it `ok`. These assert the
 * two properties that makes a digest worth having — it moves when the compiled
 * sources move, and it does not move when something that cannot reach the
 * binaries moves — plus that every way of *not knowing* stays out of `ok`.
 */

const temps: string[] = [];
afterEach(() => {
  for (const dir of temps.splice(0)) rmSync(dir, { recursive: true, force: true });
});

/** A miniature crate laid out the way `sourceFiles` expects to find one. */
function tree(files: Record<string, string>): string {
  const root = mkdtempSync(path.join(os.tmpdir(), "gp-identity-"));
  temps.push(root);
  for (const [rel, body] of Object.entries(files)) {
    const full = path.join(root, rel);
    mkdirSync(path.dirname(full), { recursive: true });
    writeFileSync(full, body);
  }
  return root;
}

const BASE = {
  "src-tauri/Cargo.toml": "[package]\nname = \"gitpulse\"\n",
  "src-tauri/src/main.rs": "fn main() {}\n",
  "src-tauri/src/hooks/mod.rs": "pub const VERSION: &str = \"1.1.0\";\n",
};

describe("source digest", () => {
  it("is stable when nothing changed", () => {
    const root = tree(BASE);
    expect(sourceDigest(root).digest).toBe(sourceDigest(root).digest);
  });

  it("moves when a compiled source file changes by one byte", () => {
    const root = tree(BASE);
    const before = sourceDigest(root).digest;
    writeFileSync(path.join(root, "src-tauri/src/hooks/mod.rs"), "pub const VERSION: &str = \"1.1.1\";\n");
    expect(sourceDigest(root).digest).not.toBe(before);
  });

  it("moves when a file is renamed, even though the bytes are unchanged", () => {
    // The path goes into the stream for this reason: `mod.rs` moving to
    // `other.rs` changes what compiles while leaving the content identical.
    const a = tree(BASE);
    const b = tree({
      "src-tauri/Cargo.toml": BASE["src-tauri/Cargo.toml"],
      "src-tauri/src/main.rs": BASE["src-tauri/src/main.rs"],
      "src-tauri/src/hooks/other.rs": BASE["src-tauri/src/hooks/mod.rs"],
    });
    expect(sourceDigest(a).digest).not.toBe(sourceDigest(b).digest);
  });

  it("moves when a byte crosses a file boundary", () => {
    // Hashing concatenated contents alone would miss this; the length field is
    // what makes ("ab","c") and ("a","bc") different streams.
    const a = tree({ ...BASE, "src-tauri/src/a.rs": "ab", "src-tauri/src/b.rs": "c" });
    const b = tree({ ...BASE, "src-tauri/src/a.rs": "a", "src-tauri/src/b.rs": "bc" });
    expect(sourceDigest(a).digest).not.toBe(sourceDigest(b).digest);
  });

  it("ignores what cannot reach the binaries, so it is not a nuisance", () => {
    const root = tree(BASE);
    const before = sourceDigest(root).digest;
    mkdirSync(path.join(root, "src-tauri/tests"), { recursive: true });
    writeFileSync(path.join(root, "src-tauri/tests/huge.rs"), "#[test] fn t() {}\n");
    mkdirSync(path.join(root, "src-tauri/target/debug"), { recursive: true });
    writeFileSync(path.join(root, "src-tauri/target/debug/build.rs"), "fn main() {}\n");
    expect(sourceDigest(root).digest).toBe(before);
  });

  it("covers Swift, which build.rs compiles into the binary", () => {
    const root = tree({ ...BASE, "src-tauri/swift/AppleIntelligence.swift": "import Foundation\n" });
    const before = sourceDigest(root).digest;
    writeFileSync(path.join(root, "src-tauri/swift/AppleIntelligence.swift"), "import Foundation\n// x\n");
    expect(sourceDigest(root).digest).not.toBe(before);
  });

  it("refuses an empty walk rather than hashing nothing into a stable value", () => {
    // A digest over no files would compare equal to itself forever, which is a
    // check that agrees with everything it is asked.
    const root = tree({ "src-tauri/README.md": "not a source file\n" });
    expect(() => sourceDigest(root)).toThrow(/no source files/);
  });

  it("finds a non-trivial set in this repository, so the checks above are not vacuous", () => {
    const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
    const files = sourceFiles(repo);
    expect(files.length).toBeGreaterThan(100);
    expect(files.every((f) => !f.includes("/target/") && !f.includes("/tests/"))).toBe(true);
    // Ascending and `/`-separated, so the stream does not depend on the
    // filesystem's enumeration order or on the platform's separator.
    expect(files).toStrictEqual([...files].sort());
    expect(files.some((f) => f.includes("\\"))).toBe(false);
  });
});

describe("install record provenance", () => {
  const record = (over: Partial<Parameters<typeof classifyProvenance>[0]["record"]> = {}) => ({
    version: RECORD_VERSION,
    installedAt: "2026-09-14T00:00:00.000Z",
    sourceRoot: "/repo",
    sourceDigest: "a".repeat(64),
    sourceFileCount: 3,
    binaries: {},
    ...over,
  });

  /** A real file so `fileDigest` has something to read. */
  function binary(body: string): { path: string; digest: string } {
    const root = tree({ "src-tauri/src/main.rs": "fn main() {}\n" });
    const file = path.join(root, "gitpulse-hook");
    writeFileSync(file, body);
    return { path: file, digest: fileDigest(file) as string };
  }

  it("reports unverifiable when nothing was ever recorded", () => {
    const verdict = classifyProvenance({
      record: null, treeDigest: "a".repeat(64), treeFileCount: 3, binPaths: ["/usr/bin/x"], root: "/repo",
    });
    // Not `ok`: an install nobody recorded looked exactly like one that was
    // checked and matched, which is the whole defect being closed here.
    expect(verdict.status).toBe("unverifiable");
    expect(verdict.violations.join(" ")).toMatch(/npm run mcp:install/);
  });

  it("reports unverifiable when no binary is on PATH at all", () => {
    const verdict = classifyProvenance({
      record: record(), treeDigest: "a".repeat(64), treeFileCount: 3, binPaths: [null, null], root: "/repo",
    });
    expect(verdict.status).toBe("unverifiable");
  });

  it("reports unverifiable for a binary the record does not describe", () => {
    const bin = binary("#!/bin/sh\n");
    const verdict = classifyProvenance({
      record: record(), treeDigest: "a".repeat(64), treeFileCount: 3, binPaths: [bin.path], root: "/repo",
    });
    expect(verdict.status).toBe("unverifiable");
    expect(verdict.violations.join(" ")).toMatch(/does not describe it/);
  });

  it("reports unverifiable when the binary changed after it was recorded", () => {
    // Someone ran a bare `cargo install`, or copied a build in by hand. We
    // cannot say what source that binary holds, so we must not say it matches.
    const bin = binary("#!/bin/sh\n");
    writeFileSync(bin.path, "#!/bin/sh\n# rebuilt elsewhere\n");
    const verdict = classifyProvenance({
      record: record({ binaries: { [bin.path]: bin.digest } }),
      treeDigest: "a".repeat(64), treeFileCount: 3, binPaths: [bin.path], root: "/repo",
    });
    expect(verdict.status).toBe("unverifiable");
    expect(verdict.violations.join(" ")).toMatch(/has changed since it was recorded/);
  });

  it("reports stale when the tree has moved on from what was installed", () => {
    const bin = binary("#!/bin/sh\n");
    const verdict = classifyProvenance({
      record: record({ binaries: { [bin.path]: bin.digest } }),
      treeDigest: "b".repeat(64), treeFileCount: 4, binPaths: [bin.path], root: "/repo",
    });
    expect(verdict.status).toBe("stale");
    expect(verdict.violations.join(" ")).toMatch(/differs from this tree/);
  });

  it("names the other checkout when the install came from one", () => {
    const bin = binary("#!/bin/sh\n");
    const verdict = classifyProvenance({
      record: record({ binaries: { [bin.path]: bin.digest }, sourceRoot: "/elsewhere" }),
      treeDigest: "b".repeat(64), treeFileCount: 4, binPaths: [bin.path], root: "/repo",
    });
    expect(verdict.violations.join(" ")).toMatch(/installed from \/elsewhere/);
  });

  it("is ok only when the binary and the sources both match", () => {
    const bin = binary("#!/bin/sh\n");
    const verdict = classifyProvenance({
      record: record({ binaries: { [bin.path]: bin.digest } }),
      treeDigest: "a".repeat(64), treeFileCount: 3, binPaths: [bin.path, null], root: "/repo",
    });
    expect(verdict.status).toBe("ok");
    expect(verdict.violations).toStrictEqual([]);
  });
});

describe("what the recorder records", () => {
  it("derives the binary names from the install script, and finds some", () => {
    // Parsed from `mcp:install` exactly as plugin-contract.test.ts parses it,
    // so a binary added to the install is recorded without editing that file.
    // An empty parse must throw rather than write down an empty record.
    const names = installedBinNames();
    expect(names.length).toBeGreaterThan(0);
    expect(names).toContain("gitpulse-hook");
    expect(names).toContain("gitpulse-mcp");
  });
});
