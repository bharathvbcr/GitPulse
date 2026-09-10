import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * Windows `cargo test --lib` builds `gitpulse_lib-*.exe`. tauri-winres links
 * its resource only as `rustc-link-arg-bins`, and `rustc-link-arg-tests` only
 * covers `tests/*.rs`, so the lib harness died at load with
 * STATUS_ENTRYPOINT_NOT_FOUND (0xc0000139) — no test name, nothing checked.
 *
 * Catch-all `/MANIFEST:EMBED` reaches that harness, but the same flag on bins
 * while tauri-winres still embeds RT_MANIFEST is CVT1100 (duplicate resource).
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

  it("embeds comctl32 once via catch-all link args, not a second RT_MANIFEST on bins", () => {
    expect(buildRs).toContain("new_without_app_manifest");
    expect(buildRs).toContain("try_build");
    expect(buildRs).not.toMatch(/tauri_build::build\s*\(\s*\)/);
    expect(buildRs).toContain("tests.manifest");
    expect(buildRs).toContain('println!("cargo:rustc-link-arg=/MANIFEST:EMBED")');
    expect(buildRs).toContain("cargo:rustc-link-arg=/MANIFESTINPUT:");
    expect(buildRs).not.toMatch(/println!\("cargo:rustc-link-arg-tests=/);
    expect(buildRs).not.toMatch(/println!\("cargo:rustc-link-arg-bins=/);
    expect(buildRs).not.toMatch(/println!\("cargo:rustc-cdylib-link-arg=/);
  });

  it("keeps XML comments free of -- so mt.exe can parse the manifest", () => {
    const comments = [...manifest.matchAll(/<!--([\s\S]*?)-->/g)].map((m) => m[1]);
    expect(comments.length).toBeGreaterThan(0);
    for (const comment of comments) {
      expect(comment, "XML comments cannot contain -- (mt.exe c1010070)").not.toContain("--");
    }
  });
});
