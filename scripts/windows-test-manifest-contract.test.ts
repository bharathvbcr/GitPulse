import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * Windows `cargo test --lib` loads this package's `cdylib`. The comctl32 v6
 * manifest used to be passed only as `rustc-link-arg-tests`, so integration
 * binaries started and the lib harness died at load with
 * STATUS_ENTRYPOINT_NOT_FOUND (0xc0000139) — no test name, nothing checked.
 */
const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const cargo = readFileSync(path.join(REPO, "src-tauri", "Cargo.toml"), "utf8");
const buildRs = readFileSync(path.join(REPO, "src-tauri", "build.rs"), "utf8");
const manifest = readFileSync(path.join(REPO, "src-tauri", "tests.manifest"), "utf8");

describe("Windows test binaries and the lib cdylib share the comctl32 manifest", () => {
  it("declares a cdylib that cargo test --lib will load", () => {
    const lib = cargo.match(/\[lib\]([\s\S]*?)(?=\n\[|$)/)?.[1] ?? "";
    expect(lib).toContain("cdylib");
  });

  it("embeds the manifest on both --test binaries and the cdylib", () => {
    expect(buildRs).toContain("rustc-link-arg-tests");
    expect(buildRs).toContain("rustc-cdylib-link-arg");
    expect(buildRs).toContain("tests.manifest");
    const kinds = [...buildRs.matchAll(/"(rustc-[^"]+)"/g)].map((m) => m[1]);
    expect(kinds).toEqual(
      expect.arrayContaining([
        "rustc-link-arg-tests",
        "rustc-cdylib-link-arg",
        "rustc-link-arg-bins",
        "rustc-link-arg",
      ]),
    );
  });

  it("keeps XML comments free of -- so mt.exe can parse the manifest", () => {
    const comments = [...manifest.matchAll(/<!--([\s\S]*?)-->/g)].map((m) => m[1]);
    expect(comments.length).toBeGreaterThan(0);
    for (const comment of comments) {
      expect(comment, "XML comments cannot contain -- (mt.exe c1010070)").not.toContain("--");
    }
  });
});
