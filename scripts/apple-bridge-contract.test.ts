/**
 * How the Apple Intelligence bridge is allowed to be linked.
 *
 * Both rules here were written after the build broke something else, and
 * neither failure pointed at this file:
 *
 *  1. **The Swift shim is built for macOS 26, not the crate's floor.** Built
 *     for an older target, Swift force-links its back-deployment shims
 *     (`swiftCompatibilityConcurrency` and friends), which install global
 *     executor hooks into the *whole process*. That measurably broke an
 *     unrelated PTY reader — `terminal_pty_stress`'s
 *     `close_wakes_a_reader_blocked_on_a_full_output_window` stopped receiving
 *     its 252 KiB flood within five seconds, reproducibly, while the same tree
 *     with the bridge linked out passed 10/10. Every entry point in the shim
 *     is `@available(macOS 26.0, *)` guarded, so the older target bought
 *     nothing and cost that.
 *
 *  2. **The framework is weak-linked.** `FoundationModels` first exists on
 *     macOS 26. A hard `-framework` reference becomes an `LC_LOAD_DYLIB` load
 *     command, so dyld would refuse to start GitPulse at all on any older
 *     macOS — for a feature those users cannot reach anyway.
 *
 * Neither rule is visible in the Rust or Swift source, which is exactly why it
 * is asserted here rather than left to a comment.
 */
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const buildRs = readFileSync(path.join(REPO_ROOT, "src-tauri", "build.rs"), "utf8");
const swift = readFileSync(path.join(REPO_ROOT, "src-tauri", "swift", "AppleIntelligence.swift"), "utf8");
/** Only the lines build.rs actually prints to cargo; comments are not rules. */
const directives = [...buildRs.matchAll(/println!\("(cargo:[^"]*)"/g)].map((match) => match[1]);

describe("the Apple Intelligence bridge links without changing the rest of the process", () => {
  it("builds the Swift shim for macOS 26, so no back-deployment shim is force-linked", () => {
    expect(buildRs).toContain('let deployment = "26.0"');
    expect(buildRs).toContain('.args(["-target", &format!("{arch}-apple-macosx{deployment}")])');
    // Asserted against what build.rs *emits*, not its prose: the compatibility
    // archives are named in the comment above, on purpose, so a reader knows
    // what this rule is protecting against.
    expect(directives.some((line) => line.includes("lib/swift/macosx"))).toBe(false);
    expect(directives.some((line) => line.includes("swiftCompatibility"))).toBe(false);
  });

  it("weak-links the framework so an older macOS can still launch GitPulse", () => {
    expect(directives.some((line) => line.includes("-Wl,-weak_framework,FoundationModels"))).toBe(true);
    // A plain `-l framework=` would be the hard dependency this avoids.
    expect(directives.some((line) => line.includes("link-lib=framework=FoundationModels"))).toBe(false);
  });

  it("guards every framework entry point behind an availability check", () => {
    // Weak linking only helps if nothing touches the null symbols. Each
    // `@_cdecl` export is reachable from Rust on any macOS, so each one has to
    // check before it reaches the framework.
    const exports = [...swift.matchAll(/@_cdecl\("([^"]+)"\)/g)].map((match) => match[1]);
    expect(exports).toEqual([
      "gitpulse_apple_intelligence_status",
      "gitpulse_apple_intelligence_generate",
      "gitpulse_apple_intelligence_free",
    ]);
    // `free` touches no framework symbol; the other two must gate.
    const gating = swift.match(/#available\(macOS 26\.0, \*\)/g) ?? [];
    expect(gating.length).toBeGreaterThanOrEqual(2);
    expect(swift).toContain("#if canImport(FoundationModels)");
  });

  it("keeps the bridge optional at build time, and honest when it is absent", () => {
    // A host without the framework must produce a binary that says so, rather
    // than one that reports the user's Mac as unable.
    expect(buildRs).toContain("cargo:rustc-check-cfg=cfg(apple_intelligence)");
    expect(buildRs).toContain("GITPULSE_DISABLE_APPLE_INTELLIGENCE");
    expect(buildRs).toContain('xcrun(&["--sdk", "macosx", "--show-sdk-path"])');
    expect(buildRs).toContain("FoundationModels.framework");
    // Every skip path warns; a silent skip is how a release ships without the
    // feature and nobody finds out until a user asks for it.
    const skips = buildRs.match(/cargo:warning=Apple Intelligence bridge/g) ?? [];
    expect(skips.length).toBeGreaterThanOrEqual(5);
  });
});
