import { readFileSync } from "node:fs";
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
const NPM_LOCK = JSON.parse(readFileSync(join(REPO, "package-lock.json"), "utf8")) as {
  packages: Record<string, { name?: string; version?: string }>;
};

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

  it("locks wry 0.56.x from the unpublished Tauri 2.12 line", () => {
    expect(packageVersion("wry")).toMatch(/^0\.56\./);
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

  it("installs the Health-scan npm refreshes", () => {
    expect(PKG.dependencies?.["@lucide/svelte"]).toBe("^1.44.0");
    expect(NPM_LOCK.packages["node_modules/@lucide/svelte"]?.version).toBe("1.44.0");
    expect(PKG.devDependencies?.vite).toBe("^8.3.0");
    expect(NPM_LOCK.packages["node_modules/vite"]?.version).toBe("8.3.0");
    expect(PKG.devDependencies?.["@types/node"]).toBe("^26.5.1");
    expect(NPM_LOCK.packages["node_modules/@types/node"]?.version).toBe("26.5.1");
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

    const jscValue = readFileSync(join(framework, "javascriptcore-rs", "src", "value.rs"), "utf8");
    expect(jscValue).toMatch(/fn typed_array_get_data\(&self\) -> TypedArrayData<'_>/);
    const jscAuto = readFileSync(join(framework, "javascriptcore-rs", "src", "auto", "mod.rs"), "utf8");
    expect(jscAuto).toMatch(/^pub mod builders \{/m);

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
    expect(NPM_LOCK.packages["node_modules/@typescript/native"]?.version).toBe("7.0.2");
    expect(NPM_LOCK.packages["node_modules/typescript"]?.name).toBe("@typescript/typescript6");
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
