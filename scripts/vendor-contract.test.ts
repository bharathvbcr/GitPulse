import { existsSync, readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { check, main, MANIFEST, readToml, resolveManifest, sources, VENDOR_DIR } from "./vendor-crates.mjs";

/**
 * GitPulse must build from a checkout of GitPulse.
 *
 * It used to reach `../../../../../Manvi/crates/…` and
 * `../../../../../DevCouncil/rust-port/crates/…`, so a lone clone did not
 * build: it needed two unrelated repositories present, at the right depth, on
 * every machine and every CI runner. The crates are vendored under
 * `src-tauri/vendored/` now, and this is what keeps them that way — a single
 * `path = "../.."` added back is a one-line change that nothing else notices
 * until a fresh clone fails.
 */
const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const CARGO_TOML = path.join(REPO, "src-tauri", "Cargo.toml");

/** Every `path = "…"` in a Cargo manifest, in source order. */
function pathDependencies(manifest: string): string[] {
  return [...manifest.matchAll(/\bpath\s*=\s*"([^"]+)"/g)].map((m) => m[1]);
}

describe("GitPulse builds standalone", () => {
  const manifest = readFileSync(CARGO_TOML, "utf8");

  it("has path dependencies to check", () => {
    // A regex that matched nothing would make every assertion below vacuous.
    expect(pathDependencies(manifest).length).toBeGreaterThan(0);
  });

  it("has no path dependency that leaves the repository", () => {
    for (const dep of pathDependencies(manifest)) {
      expect(dep.startsWith(".."), `${dep} reaches outside src-tauri`).toBe(false);
      const resolved = path.resolve(path.dirname(CARGO_TOML), dep);
      expect(resolved.startsWith(REPO + path.sep), `${dep} resolves outside the repository`).toBe(true);
      expect(existsSync(path.join(resolved, "Cargo.toml")), `${dep} has no manifest`).toBe(true);
    }
  });

  it("resolves every vendored crate's own path dependencies inside the vendor tree", () => {
    // dc-verify depends on dc-glob, and the devmap crates on each other. Those
    // are `../<name>`, which is only correct because the copies sit as
    // siblings — vendoring one and not the others would break here.
    for (const crate of readdirSync(VENDOR_DIR).filter((e) => existsSync(path.join(VENDOR_DIR, e, "Cargo.toml")))) {
      const dir = path.join(VENDOR_DIR, crate);
      for (const dep of pathDependencies(readFileSync(path.join(dir, "Cargo.toml"), "utf8"))) {
        const resolved = path.resolve(dir, dep);
        expect(resolved.startsWith(VENDOR_DIR + path.sep), `${crate} reaches ${dep}`).toBe(true);
        expect(existsSync(path.join(resolved, "Cargo.toml")), `${crate} depends on missing ${dep}`).toBe(true);
      }
    }
  });

  it("leaves no workspace inheritance in a vendored manifest", () => {
    // These crates come out of workspaces whose roots are not here. An
    // inherited key that survived would fail at `cargo build` with an error
    // about a manifest this repository appears to own.
    for (const crate of readdirSync(VENDOR_DIR).filter((e) => existsSync(path.join(VENDOR_DIR, e, "Cargo.toml")))) {
      const text = readFileSync(path.join(VENDOR_DIR, crate, "Cargo.toml"), "utf8");
      const offending = text
        .split("\n")
        .filter((line) => /\bworkspace\b/.test(line) && !line.trim().startsWith("#"));
      expect(offending, `${crate} still inherits from a workspace`).toEqual([]);
    }
  });
});

describe("the vendored tree matches what was recorded", () => {
  it("records every crate on disk, and no others", () => {
    const recorded = new Set(JSON.parse(readFileSync(MANIFEST, "utf8")).crates.map((c: { name: string }) => c.name));
    const onDisk = new Set(
      readdirSync(VENDOR_DIR).filter((e) => existsSync(path.join(VENDOR_DIR, e, "Cargo.toml"))),
    );
    expect([...onDisk].sort()).toEqual([...recorded].sort());
  });

  it("finds no vendored file edited here", () => {
    // Editing these copies is how they quietly stop being copies. Fixes belong
    // upstream, followed by a re-vendor.
    const result = check();
    for (const crate of result.crates) {
      expect(crate.edited, `${crate.name} has been edited in this repository`).toEqual([]);
    }
  });

  it("names the upstream commit each crate came from", () => {
    // Without it, "matches upstream" has no upstream to mean anything against.
    for (const crate of JSON.parse(readFileSync(MANIFEST, "utf8")).crates) {
      expect(crate.origin.commit, `${crate.name} records no upstream commit`).toMatch(/^[0-9a-f]{40}$/);
    }
  });

  it("records what it left behind rather than leaving it to be inferred", () => {
    for (const crate of JSON.parse(readFileSync(MANIFEST, "utf8")).crates) {
      expect(crate.omitted).toContain("tests/");
      expect(crate.omitted).toContain("[dev-dependencies]");
    }
  });
});

describe("an upstream it cannot see is not an upstream that agrees", () => {
  /**
   * The invariant the check mode exists for. Comparing against a repository
   * that is not checked out is impossible, and the tempting shape — treat "no
   * files differed" as "matches" — reports a comparison that never ran as a
   * clean one.
   */
  it("reports unavailable, never matches, when the sibling is absent", () => {
    const result = check({ GITPULSE_MANVI_ROOT: "/nonexistent/manvi", GITPULSE_DEVCOUNCIL_ROOT: "/nonexistent/dc" });
    expect(result.crates.length).toBeGreaterThan(0);
    for (const crate of result.crates) {
      expect(crate.upstream, `${crate.name}`).toBe("unavailable");
      expect(crate.reason).toContain("not checked out");
    }
    expect(result.comparable).toBe(false);
    // Still true, and still worth saying: the local-edit half genuinely ran.
    expect(result.ok).toBe(true);
  });

  it("says out loud that an incomparable run is not a clean bill of health", () => {
    const logged: string[] = [];
    const original = console.log;
    console.log = (...args: unknown[]) => void logged.push(args.join(" "));
    try {
      process.env.GITPULSE_MANVI_ROOT = "/nonexistent/manvi";
      process.env.GITPULSE_DEVCOUNCIL_ROOT = "/nonexistent/dc";
      expect(main(["--check"])).toBe(0);
    } finally {
      console.log = original;
      delete process.env.GITPULSE_MANVI_ROOT;
      delete process.env.GITPULSE_DEVCOUNCIL_ROOT;
    }
    expect(logged.join("\n")).toContain("not a clean bill of health");
  });

  it("emits the same verdict as JSON, and suppresses the prose", () => {
    const capture = (argv: string[]) => {
      const logged: string[] = [];
      const original = console.log;
      console.log = (...args: unknown[]) => void logged.push(args.join(" "));
      try {
        return { code: main(argv), text: logged.join("\n") };
      } finally {
        console.log = original;
      }
    };
    const json = capture(["--check", "--json"]);
    const text = capture(["--check"]);
    expect(json.code).toBe(text.code);
    const parsed = JSON.parse(json.text);
    expect(parsed.crates.length).toBeGreaterThan(0);
    expect(json.text).not.toContain("local:");
  });

  it("refuses an option it does not know rather than ignoring it", () => {
    const original = console.error;
    console.error = () => {};
    try {
      expect(main(["--dry-run"])).toBe(2);
    } finally {
      console.error = original;
    }
  });
});

describe("manifest inheritance is resolved, not guessed", () => {
  const workspace = readToml(`
[workspace]
resolver = "2"
members = [
    "crates/a",
    "crates/b",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "MIT"

[workspace.dependencies]
serde = { version = "1.0", features = ["derive"] }
anyhow = "1.0"

[workspace.lints.rust]
unsafe_code = "forbid"
`);

  it("reads a value that runs over several lines", () => {
    // `members` spans four lines. Taking only the first would have parsed it
    // as `[`, which is not an error and not the value.
    expect(workspace.get("workspace")?.get("members")).toBe(`[ "crates/a", "crates/b", ]`);
  });

  it("substitutes package keys and dependency specs", () => {
    const { text, rewrites } = resolveManifest(
      `[package]\nname = "a"\nversion.workspace = true\nedition.workspace = true\n\n[dependencies]\nserde.workspace = true\nanyhow = { workspace = true, optional = true }\n`,
      workspace,
    );
    expect(text).toContain(`version = "0.1.0"`);
    expect(text).toContain(`edition = "2021"`);
    expect(text).toContain(`serde = { version = "1.0", features = ["derive"] }`);
    // A bare string spec becomes a table so the extras have somewhere to go,
    // and `optional` survives — dropping it would make an optional dependency
    // mandatory and quietly pull in what the feature exists to exclude.
    expect(text).toContain(`anyhow = { version = "1.0", optional = true }`);
    expect(rewrites.length).toBe(4);
  });

  it("inlines the workspace's lints rather than dropping them", () => {
    // `[lints] workspace = true` silently discarded would turn `unsafe_code =
    // "forbid"` into permission.
    const { text } = resolveManifest(`[package]\nname = "a"\nversion.workspace = true\n\n[lints]\nworkspace = true\n`, workspace);
    expect(text).toContain("[lints.rust]");
    expect(text).toContain(`unsafe_code = "forbid"`);
  });

  it("drops dev-dependencies, which is why tests are not vendored either", () => {
    const { text, rewrites } = resolveManifest(
      `[package]\nname = "a"\nversion.workspace = true\n\n[dev-dependencies]\nsomething = { path = "../not-vendored" }\n`,
      workspace,
    );
    expect(text).not.toContain("not-vendored");
    expect(rewrites).toContain("dropped [dev-dependencies]");
  });

  it("refuses an inheritance it does not understand", () => {
    // Passing it through would surface later as a cargo error about a manifest
    // this script reported it had already fixed.
    expect(() =>
      resolveManifest(`[package]\nname = "a"\nbadges.workspace = true\n`, workspace),
    ).toThrow(/unhandled inheritance/);
    expect(() =>
      resolveManifest(`[package]\nname = "a"\nrust-version.workspace = true\n`, workspace),
    ).toThrow(/does not define/);
    expect(() =>
      resolveManifest(`[package]\nname = "a"\n\n[dependencies]\nmissing.workspace = true\n`, workspace),
    ).toThrow(/does not declare it/);
  });

  it("leaves everything it does not have to change byte-identical", () => {
    const input = `[package]\nname = "a"\nversion = "9.9.9"\n\n[features]\ndefault = ["parse"]\nparse = [\n    "dep:x",\n]\n`;
    expect(resolveManifest(input, workspace).text).toBe(input);
  });
});

describe("the source table", () => {
  it("lets both upstream roots be overridden", () => {
    const configured = sources({ GITPULSE_MANVI_ROOT: "/a", GITPULSE_DEVCOUNCIL_ROOT: "/b" });
    expect(configured.map((s) => s.root)).toEqual(["/a", "/b"]);
  });

  it("covers every crate that is vendored", () => {
    const declared = sources().flatMap((s) => s.crates).sort();
    const onDisk = readdirSync(VENDOR_DIR)
      .filter((e) => existsSync(path.join(VENDOR_DIR, e, "Cargo.toml")))
      .sort();
    expect(onDisk).toEqual(declared);
  });
});
