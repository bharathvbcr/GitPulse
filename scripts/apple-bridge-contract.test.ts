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
/** The two validators that reject an unknown drafting kind before the bridge runs. */
const appleRs = readFileSync(path.join(REPO_ROOT, "src-tauri", "src", "ai", "apple.rs"), "utf8");
const appleTs = readFileSync(path.join(REPO_ROOT, "src", "lib", "ai", "appleIntelligence.ts"), "utf8");
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

/**
 * Every drafting kind keeps the author's meaning.
 *
 * Preserving intent used to be one branch's clause, so the two kinds that need
 * it most went without it: `draft` writes a task from raw notes, and `extract`
 * is under explicit instruction to leave material out. The rule now lives in
 * the `shared` block every branch is built from, and the point of asserting it
 * *structurally* — rather than checking three strings — is that adding a
 * fourth kind cannot reintroduce the gap.
 *
 * The kind list is derived from the two validators that actually reject an
 * unknown kind, not written down here, and they are cross-checked against each
 * other so a broken parse fails instead of passing empty.
 */
const rustKinds = [
  ...(appleRs.match(/matches!\(\s*request\.kind\.as_str\(\),([^)]*)\)/)?.[1] ?? "").matchAll(/"([a-z_]+)"/g),
].map((match) => match[1]!);
const tsKinds = [
  ...(appleTs.match(/^\s*kind:\s*((?:"[a-z_]+"\s*\|\s*)*"[a-z_]+")\s*;/m)?.[1] ?? "").matchAll(/"([a-z_]+)"/g),
].map((match) => match[1]!);
/** `instructions(for:)` only — the rest of the file also says `return`. */
const instructions = swift.slice(
  swift.indexOf("private func instructions(for kind: String) -> String {"),
  swift.indexOf("private func prompt(for request: Request"),
);

describe("every Apple Intelligence drafting kind preserves the author's meaning", () => {
  it("derives the same kinds from the Rust and TypeScript validators", () => {
    // Non-vacuity: an empty parse on either side is a broken test, not a pass.
    expect(rustKinds.length).toBeGreaterThanOrEqual(2);
    expect([...tsKinds].sort()).toEqual([...rustKinds].sort());
    expect(instructions).not.toBe("");
  });

  it("states the preservation rule once, in the block every kind is built from", () => {
    const shared = instructions.match(/let shared = """([\s\S]*?)"""/)?.[1] ?? "";
    expect(shared).toContain("Preserve the author's intent and message");
    // The specifics matter more than the slogan: a model that keeps the "gist"
    // still rewrites the error code the note was written to record.
    expect(shared).toMatch(/keep their meaning and their\s*\\?\s*terminology/);
    expect(shared).toMatch(/reproduce exactly any error code/);
  });

  it("lets a kind add to the shared rules but never replace them", () => {
    const returns = [...instructions.matchAll(/\breturn\s+(\w+)/g)].map((match) => match[1]);
    expect(returns.length).toBeGreaterThanOrEqual(rustKinds.length);
    expect(new Set(returns)).toEqual(new Set(["shared"]));
  });

  it("does not let a @Guide contradict the rules the instructions state", () => {
    // A @Guide steers the decoder token by token; the instructions are only
    // something the model read. Where they disagree the guide wins, so a guide
    // that forces padding or forces brevity silently repeals the rule above it.
    const guides = [...swift.matchAll(/@Guide\(\s*description:\s*\n?\s*"((?:[^"\\]|\\.)*)"/g)].map((m) => m[1]!);
    expect(guides).toHaveLength(2);
    const [title, description] = guides as [string, string];
    // A word budget is what the model spends by rewording an identifier.
    expect(title).not.toMatch(/at most \d+ words/);
    // State the bound that is actually enforced, in the unit it is enforced in.
    expect(title).toContain("at most 300 characters");
    expect(title).toMatch(/never buy that brevity by dropping or rewording/);
    // A sentence floor on a thin note is an instruction to invent.
    expect(description).not.toMatch(/\b(?:two|three|four) to (?:three|four|five)\b/i);
    expect(description).toMatch(/only as many as the notes actually support/);
    for (const guide of guides) expect(guide).toMatch(/error code/);
  });

  it("bounds a reply in the unit every reader downstream counts in", () => {
    // Swift's `prefix` counts grapheme clusters; `interpret` and the store both
    // count Unicode scalars. One family emoji is 1 grapheme and 5 scalars, so a
    // grapheme-based cut could hand back 1500 scalars for a 300 bound — refused
    // by the store after the model had already run — while on plain ASCII it
    // landed exactly on the bound and made that refusal unreachable.
    expect(swift).toContain("toScalars");
    expect(swift).toMatch(/text\.unicodeScalars\.count > limit/);
    expect(swift).not.toMatch(/text\.count > limit/);
    // The bound Swift cuts to is the one Rust refuses past.
    const refusal = appleRs.match(/name == "title" && text\.chars\(\)\.count\(\) > (\d+)/)?.[1];
    expect(refusal).toBeDefined();
    expect(swift).toContain(`clean(content.title, limit: ${refusal})`);
  });

  it("tells the model which field it must leave alone, and only when there is one", () => {
    const body = swift.slice(
      swift.indexOf("private func prompt(for request: Request"),
      swift.indexOf("private func trimmed("),
    );
    expect(body).not.toBe("");
    expect(body).toMatch(/stays exactly as it is/);
    // Conditional on the field actually being in the prompt: telling a model to
    // leave a title alone when none was supplied asserts one exists.
    expect(body).toMatch(/currentDescription == nil/);
    expect(body).toMatch(/currentTitle == nil/);
  });

  it("cases only kinds the validators accept, and defaults the rest", () => {
    const cased = [...instructions.matchAll(/case "([a-z_]+)":/g)].map((match) => match[1]!);
    expect(cased.length).toBeGreaterThan(0);
    for (const kind of cased) expect(rustKinds).toContain(kind);
    // Whatever is not cased reaches `default`, which must therefore exist.
    if (rustKinds.some((kind) => !cased.includes(kind))) expect(instructions).toContain("default:");
  });
});
