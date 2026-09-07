import { readFileSync } from "node:fs";
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
  dependencies?: Record<string, string>;
  devDependencies?: Record<string, string>;
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

  it("uses @lucide/svelte, not the deprecated lucide-svelte package", () => {
    expect(PKG.dependencies?.["@lucide/svelte"]).toBeDefined();
    expect(PKG.dependencies?.["lucide-svelte"]).toBeUndefined();
  });
});
