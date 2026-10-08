import { existsSync, readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * The unic-* crates (RUSTSEC-2025-0075/0080/0081/0098/0100) arrived through
 * urlpattern 0.3. urlpattern 0.6 dropped them. If Cargo.lock ever rolls back
 * to 0.3, cargo-audit grows those findings again and nothing in the app
 * would notice until the next Health scan.
 */
const REPO = join(dirname(fileURLToPath(import.meta.url)), "..");
const LOCK = readFileSync(join(REPO, "src-tauri", "Cargo.lock"), "utf8");
const PKG = JSON.parse(readFileSync(join(REPO, "package.json"), "utf8")) as {
  scripts: Record<string, string>;
  dependencies?: Record<string, string>;
  devDependencies?: Record<string, string>;
};
// bun.lock is JSON with trailing commas. Each `packages` entry is a tuple whose
// first element is the resolved `name@version` (the real name for an alias).
const BUN_LOCK = JSON.parse(
  readFileSync(join(REPO, "bun.lock"), "utf8").replace(/,(\s*[}\]])/g, "$1"),
) as { packages: Record<string, [string, ...unknown[]]> };

/** The resolved `name@version` bun.lock installs at node_modules/<key>. */
function locked(key: string): string | undefined {
  return BUN_LOCK.packages[key]?.[0];
}

/** `version` is a plain `x.y.z` no lower than `floor`, compared numerically. */
function atLeast(version: string | undefined, floor: string): boolean {
  const parse = (v: string) => (/^\d+\.\d+\.\d+$/.test(v) ? v.split(".").map(Number) : undefined);
  const have = version === undefined ? undefined : parse(version);
  const need = parse(floor);
  if (!have || !need) return false;
  for (let i = 0; i < 3; i += 1) {
    if (have[i] !== need[i]) return have[i] > need[i];
  }
  return true;
}

function packageVersion(name: string): string | undefined {
  const blocks = LOCK.split("[[package]]\n");
  for (const block of blocks) {
    const n = /^name = "([^"]+)"/m.exec(block)?.[1];
    if (n === name) return /^version = "([^"]+)"/m.exec(block)?.[1];
  }
  return undefined;
}

describe("advisory-sensitive lockfiles stay on the fixed parents", () => {
  it("reads real package blocks, so a missing crate cannot pass vacuously", () => {
    expect(packageVersion("urlpattern"), "urlpattern is in Cargo.lock").toBeDefined();
    expect(packageVersion("wry"), "wry is in Cargo.lock").toBeDefined();
  });

  it("locks urlpattern 0.6, which does not depend on unic-*", () => {
    expect(packageVersion("urlpattern")).toBe("0.6.0");
    expect(LOCK, "unic-ucd-ident is the urlpattern 0.3 leaf").not.toMatch(
      /^name = "unic-ucd-ident"$/m,
    );
    expect(LOCK).not.toMatch(/^name = "unic-char-property"$/m);
    expect(LOCK).not.toMatch(/^name = "unic-ucd-version"$/m);
  });

  it("locks wry 0.56 or later, the line the ported Tauri runtime was moved onto", () => {
    const wry = packageVersion("wry");
    expect(atLeast(wry, "0.56.0"), `wry ${wry} is at least 0.56.0`).toBe(true);
  });

  it("uses maintained GTK3 bindings and excludes both abandoned macro diagnostic crates", () => {
    expect(packageVersion("gtk")).toMatch(/^0\.19\./);
    expect(packageVersion("glib")).toMatch(/^0\.22\./);
    for (const block of LOCK.split("[[package]]\n")) {
      if (/^name = "glib"$/m.test(block)) {
        expect(block).toMatch(/^version = "0\.22\./m);
      }
    }
    expect(LOCK).not.toMatch(/^name = "proc-macro-error(?:2|-attr|-attr2)?"$/m);
  });

  it("uses @lucide/svelte, not the deprecated lucide-svelte package", () => {
    expect(PKG.dependencies?.["@lucide/svelte"]).toBeDefined();
    expect(PKG.dependencies?.["lucide-svelte"]).toBeUndefined();
  });

  it("compares versions numerically, so the floor check below cannot pass vacuously", () => {
    expect(atLeast("1.48.0", "1.47.0")).toBe(true);
    expect(atLeast("1.47.0", "1.47.0")).toBe(true);
    expect(atLeast("1.46.9", "1.47.0")).toBe(false);
    expect(atLeast("8.10.0", "8.3.0")).toBe(true);
    expect(atLeast("7.9.9", "8.3.0")).toBe(false);
    expect(atLeast(undefined, "8.3.0")).toBe(false);
    expect(atLeast("not-a-version", "8.3.0")).toBe(false);
  });

  // A floor, not a pin: a later Dependabot bump still carries the refresh, and
  // an exact pin turned every such bump into a red main.
  it("installs the Health-scan npm refreshes", () => {
    const floors: Array<[name: string, range: string | undefined, floor: string]> = [
      ["@lucide/svelte", PKG.dependencies?.["@lucide/svelte"], "1.47.0"],
      ["vite", PKG.devDependencies?.vite, "8.3.0"],
      ["@types/node", PKG.devDependencies?.["@types/node"], "26.6.2"],
    ];
    for (const [name, range, floor] of floors) {
      expect(range, `${name} is declared as a caret range`).toMatch(/^\^\d+\.\d+\.\d+$/);
      expect(atLeast(range?.slice(1), floor), `${name} range ${range} admits nothing below ${floor}`).toBe(true);
      const resolved = locked(name);
      expect(resolved?.startsWith(`${name}@`), `${name} is in bun.lock as ${resolved}`).toBe(true);
      expect(atLeast(resolved?.slice(name.length + 1), floor), `${resolved} is at least ${floor}`).toBe(true);
    }
  });

  it("excludes local framework ports from CodeQL default setup", () => {
    const config = readFileSync(join(REPO, ".github", "codeql", "codeql-config.yml"), "utf8");
    expect(config).toMatch(/^paths-ignore:\s*$/m);
    expect(config).toMatch(/^[ \t]+- src-tauri\/framework\/\*\*\s*$/m);
  });

  it("keeps Linux GTK ports free of the rustc lints Linux clippy -D warnings surfaces", () => {
    const framework = join(REPO, "src-tauri", "framework");
    const appIndicator = readFileSync(join(framework, "libappindicator-sys", "src", "lib.rs"), "utf8");
    expect(appIndicator).not.toMatch(/unsafe extern fn\b/);
    expect(appIndicator).toMatch(/#\[cfg\(not\(feature = "backcompat"\)\)\]\s*\n\s*panic!/s);

    // javascriptcore-rs 2.0 published the GLib 0.22 line, so its port was
    // retired: the registry crate is capped by --cap-lints like any other,
    // and the lint fixes the port carried no longer apply.
    expect(existsSync(join(framework, "javascriptcore-rs"))).toBe(false);
    expect(existsSync(join(framework, "javascriptcore-rs-sys"))).toBe(false);
    for (const name of ["javascriptcore-rs", "javascriptcore-rs-sys"]) {
      const block = LOCK.split("[[package]]\n").find((entry) => entry.startsWith(`name = "${name}"\n`));
      expect(block, `${name} is in Cargo.lock`).toBeDefined();
      expect(block).toMatch(/^version = "2\.\d+\.\d+"$/m);
      expect(block).toContain('source = "registry+https://github.com/rust-lang/crates.io-index"');
    }

    const webkit = readFileSync(join(framework, "webkit2gtk", "src", "lib.rs"), "utf8");
    expect(webkit).not.toMatch(/feature = "cargo-clippy"/);
    expect(webkit).toMatch(/^#!\[allow\(unexpected_cfgs\)\]/m);

    const wryToml = readFileSync(join(framework, "wry", "Cargo.toml"), "utf8");
    expect(wryToml).toMatch(/cfg\(feature, values\(\\?"v2_42\\?"\)\)/);
    expect(wryToml).toMatch(/cfg\(macos_12_unavailable\)/);
    expect(readFileSync(join(framework, "wry", "src", "webkitgtk", "mod.rs"), "utf8")).toMatch(
      /^#!\[allow\(deprecated\)\]/m,
    );
    expect(
      readFileSync(join(framework, "wry", "src", "webkitgtk", "synthetic_mouse_events.rs"), "utf8"),
    ).toMatch(/^#!\[allow\(deprecated\)\]/m);
  });

  it("runs stable TypeScript 7 while preserving the compiler API for Svelte and contracts", () => {
    expect(PKG.devDependencies?.["@typescript/native-preview"]).toBeUndefined();
    expect(locked("@typescript/native")).toBe("typescript@7.0.2");
    expect(locked("typescript")).toMatch(/^@typescript\/typescript6@/);
    expect(PKG.scripts.typecheck).toBe("node node_modules/@typescript/native/bin/tsc -p tsconfig.node.json --noEmit");
    // Run the actual installed CLI: a renamed dependency alone does not prove
    // the check command selects the stable compiler instead of the old API.
    const version = execFileSync(process.execPath, [join(REPO, "node_modules/@typescript/native/bin/tsc"), "--version"], {
      cwd: REPO,
      encoding: "utf8",
      timeout: 10_000,
    });
    expect(version.trim()).toBe("Version 7.0.2");
  });
});

/** `x.y` of a plain `x.y.z`, or undefined. */
function majorMinor(version: string | undefined): string | undefined {
  return /^(\d+\.\d+)\.\d+$/.exec(version ?? "")?.[1];
}

// `tauri build` refuses to run when an npm package and its Rust crate are on
// different major/minor releases. A Dependabot npm bump that outran the Rust
// side (api 2.12 over tauri 2.11) reached main that way and was only found by
// a release build. The pairs are derived from bun.lock, so a new plugin is
// covered without editing this list.
describe("npm Tauri packages and their Rust crates share a major.minor", () => {
  const pairs: Array<[npm: string, crate: string]> = [
    ["@tauri-apps/api", "tauri"],
    ...Object.keys(BUN_LOCK.packages)
      .filter((key) => /^@tauri-apps\/plugin-[a-z0-9-]+$/.test(key))
      .map((key): [string, string] => [key, `tauri-plugin-${key.slice("@tauri-apps/plugin-".length)}`]),
  ];

  it("finds the pairs it checks, so it cannot pass vacuously", () => {
    expect(pairs.map(([npm]) => npm)).toContain("@tauri-apps/api");
    expect(pairs.length, "at least one @tauri-apps/plugin-* is installed").toBeGreaterThan(1);
  });

  it.each(pairs)("%s matches the %s crate", (npm, crate) => {
    const resolved = locked(npm);
    expect(resolved, `${npm} is in bun.lock`).toBeDefined();
    const npmVersion = majorMinor(resolved?.slice(npm.length + 1));
    const crateVersion = majorMinor(packageVersion(crate));
    expect(crateVersion, `${crate} is in Cargo.lock as x.y.z`).toBeDefined();
    expect(npmVersion, `${resolved} vs ${crate} ${packageVersion(crate)}`).toBe(crateVersion);
  });
});
