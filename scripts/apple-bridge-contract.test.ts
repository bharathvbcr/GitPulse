import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * The Apple Intelligence bridge has two link-time rules that are invisible from
 * both the Rust and the Swift source, and whose breakage does not look like
 * breakage.
 *
 *  1. The framework is WEAK-linked. `FoundationModels` exists only on macOS 26.
 *     A plain `cargo:rustc-link-lib=framework=` emits `LC_LOAD_DYLIB`, a hard
 *     dependency — and a hard dependency on a framework the host does not have
 *     stops the app launching at all, with a dyld error and no GitPulse. The
 *     symptom lands on users below macOS 26, i.e. never on the machine that
 *     built it. `otool -l` on the binary must show `LC_LOAD_WEAK_DYLIB`.
 *
 *  2. The Swift is built at macOS 26, not at the crate's own floor. Building
 *     lower drags in the back-deployment compatibility shims
 *     (`libswiftCompatibility56.a` and friends). Measured on this tree: at
 *     target 11.0 the archive carries two undefined `swiftCompatibility`
 *     symbols, at 26.0 it carries none. Those shims install global-executor
 *     hooks process-wide, which is a cost paid by code with no connection to
 *     Apple Intelligence. Nothing is lost by targeting 26: the framework needs
 *     26 regardless, and every 26-only API sits behind `if #available`.
 *
 * Neither rule can be checked by the Rust type system or by a unit test, and
 * neither is apparent from reading the files it affects. This pins them at the
 * one place they are expressed.
 */
const BUILD_RS = readFileSync(
  fileURLToPath(new URL("../src-tauri/build.rs", import.meta.url)),
  "utf8",
);
const WORKBENCH_RS = readFileSync(
  fileURLToPath(new URL("../src-tauri/src/workbench.rs", import.meta.url)),
  "utf8",
);
const TASK_ORGANIZE_TS = readFileSync(
  fileURLToPath(new URL("../src/lib/workbench/taskOrganize.ts", import.meta.url)),
  "utf8",
);
/** The body of one method in workbench.rs, bounded by the next method. */
function rustMethod(name: string): string {
  const start = WORKBENCH_RS.indexOf(`    fn ${name}(`);
  if (start < 0) throw new Error(`workbench.rs has no fn ${name}`);
  const next = WORKBENCH_RS.indexOf("\n    fn ", start + 1);
  return WORKBENCH_RS.slice(start, next < 0 ? undefined : next);
}

const SWIFT = readFileSync(
  fileURLToPath(new URL("../src-tauri/apple/GitPulseAppleIntelligence.swift", import.meta.url)),
  "utf8",
);

describe("Apple Intelligence link rules", () => {
  it("weak-links FoundationModels", () => {
    expect(BUILD_RS).toContain("-Wl,-weak_framework,FoundationModels");
  });

  /** The hard form would break launch on every Mac older than 26. */
  it("never hard-links FoundationModels", () => {
    expect(BUILD_RS).not.toMatch(/rustc-link-lib=framework=FoundationModels/);
  });

  it("builds the Swift at the framework's own floor", () => {
    expect(BUILD_RS).toMatch(/SWIFT_DEPLOYMENT_TARGET: &str = "26\.0"/);
  });

  /**
   * Using the crate's deployment target here is the mistake that pulls the
   * shims back in, so the Swift target must not be read from the environment.
   */
  it("does not take the Swift target from MACOSX_DEPLOYMENT_TARGET", () => {
    expect(BUILD_RS).not.toContain("MACOSX_DEPLOYMENT_TARGET");
  });

  it("does not add the compiler's back-deployment shim directory", () => {
    // `lib/swift/macosx` beside the compiler is the shim path; `$SDK/usr/lib/swift`
    // is the runtime path and is still required.
    expect(BUILD_RS).not.toContain("lib/swift/macosx");
    expect(BUILD_RS).toContain("/usr/lib/swift");
  });

  it("asks xcrun for the Xcode SDK, not the Command Line Tools one", () => {
    // `xcrun --show-sdk-path` with no --sdk returns the CLT SDK, which has no
    // usable FoundationModels — indistinguishable from "this Mac cannot do it".
    expect(BUILD_RS).toContain('"--sdk", "macosx", "--show-sdk-path"');
  });

  it("keeps the bridge optional so a machine without Xcode still builds", () => {
    expect(BUILD_RS).toContain("GITPULSE_DISABLE_APPLE_INTELLIGENCE");
    expect(BUILD_RS).toContain("cargo:rustc-check-cfg=cfg(apple_intelligence)");
    // A missing toolchain must warn, never panic.
    expect(BUILD_RS).not.toMatch(/compile_apple_bridge\(\)\s*\.unwrap\(\)/);
  });
});

describe("Apple Intelligence bridge source", () => {
  /** Every 26-only API must be guarded, or the archive traps on older hosts. */
  it("guards framework use behind an availability check", () => {
    expect(SWIFT).toContain("if #available(macOS 26.0, *)");
    expect(SWIFT).toContain("#if canImport(FoundationModels)");
  });

  /**
   * `@_cdecl` cannot be async, so the generation call waits on a semaphore. An
   * unbounded wait would hang the calling thread for the life of the process.
   */
  it("bounds the wait for an async generation", () => {
    expect(SWIFT).toContain("DispatchSemaphore");
    expect(SWIFT).toMatch(/ready\.wait\(timeout:/);
    expect(SWIFT).not.toMatch(/\.wait\(\)\s*$/m);
  });

  /** Returned strings are strdup'd, so there must be a matching free export. */
  it("exports a free for the strings it allocates", () => {
    expect(SWIFT).toContain('@_cdecl("gitpulse_apple_string_free")');
    expect(SWIFT).toContain("strdup");
    expect(SWIFT).toMatch(/free\(pointer\)/);
  });

  it("exports exactly the three entry points Rust declares", () => {
    const exported = [...SWIFT.matchAll(/@_cdecl\("([^"]+)"\)/g)].map((m) => m[1]).sort();
    expect(exported).toEqual([
      "gitpulse_apple_availability",
      "gitpulse_apple_generate",
      "gitpulse_apple_string_free",
    ]);
  });
});

describe("the on-device provider name", () => {
  /**
   * The backend reads the provider off the stored proposal to decide whether to
   * run the model in-process or forward to the Manvi sidecar. If these two
   * strings drift apart, every on-device request is quietly forwarded to a
   * sidecar that has no provider configured — a failure that looks like a Manvi
   * misconfiguration, miles from its cause.
   */
  it("matches between Rust and TypeScript", () => {
    const rust = WORKBENCH_RS.match(/const APPLE_PROVIDER: &str = "([^"]+)"/);
    const ts = TASK_ORGANIZE_TS.match(/APPLE_ENHANCEMENT_PROVIDER = "([^"]+)"/);
    expect(rust?.[1], "Rust APPLE_PROVIDER not found").toBeTruthy();
    expect(ts?.[1], "TS APPLE_ENHANCEMENT_PROVIDER not found").toBeTruthy();
    expect(ts?.[1]).toBe(rust?.[1]);
  });

  /**
   * The on-device path must never call the store's `enhancements.generate`:
   * that transition exists to hand work to an external worker and take a lease
   * against it. Completing straight from `pending` is the supported shape.
   */
  it("completes from pending rather than taking a worker lease", () => {
    expect(WORKBENCH_RS).toContain('"enhancements.complete"');
    const onDevice = rustMethod("generate_on_device");
    expect(onDevice).toContain('"enhancements.complete"');
    expect(onDevice).not.toContain('"enhancements.generate"');
    expect(onDevice).not.toContain("worker_call");
  });

  /**
   * The model call must not happen while the store lock is held: generation
   * takes seconds, and the lock serialises every other workbench request.
   */
  it("generates outside the store lock", () => {
    const onDevice = rustMethod("generate_on_device");
    const generateAt = onDevice.indexOf("crate::ai::apple::generate");
    expect(generateAt).toBeGreaterThan(0);
    // Nothing touches the store before the model call — the proposal is handed
    // in, already read by the routing step — and the single `with_store` after
    // it is the completion. So the seconds-long generation never runs inside
    // the lock that serialises every other workbench request.
    expect(onDevice.slice(0, generateAt)).not.toContain("with_store");
    expect(onDevice.slice(generateAt).split("with_store").length - 1).toBe(1);

    // The routing read is a separate, short-lived lock in its own method, and
    // must not have grown a model call of its own.
    const routing = rustMethod("apple_enhancement");
    expect(routing).toContain("with_store");
    expect(routing).not.toContain("crate::ai::apple::generate");
  });
});
