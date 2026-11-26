import { describe, expect, it } from "vitest";
import path from "node:path";
import {
  HOMEBREW_LLD_BIN_DIRS,
  HOMEBREW_LLVM_BIN_DIRS,
  WINDOWS_TARGETS,
  buildXwinArgs,
  checkPrerequisites,
  main,
  parseArgs,
  resolveCrossEnv,
  resolveRcCompiler,
} from "./check-windows-cross.mjs";

const enoent = () => ({
  status: null,
  stdout: "",
  error: Object.assign(new Error("ENOENT"), { code: "ENOENT" }),
});

describe("check-windows-cross", () => {
  it("answers --help with exit 0 and refuses invalid or conflicting arguments with exit 2", () => {
    const logged: string[] = [];
    const origLog = console.log;
    const origErr = console.error;
    console.log = (...a: unknown[]) => void logged.push(a.join(" "));
    console.error = () => {};
    try {
      expect(main(["--help"])).toBe(0);
      expect(logged.join("\n")).toContain("Usage:");
      expect(logged.join("\n")).toContain("XWIN_CACHE_DIR");
      expect(main(["--arch", "sparc"])).toBe(2);
      expect(main(["--arch", ""])).toBe(2);
      expect(main(["--clippy", "--tests"])).toBe(2);
      expect(main(["--unknown"])).toBe(2);
    } finally {
      console.log = origLog;
      console.error = origErr;
    }
  });

  it("constructs isolated scratch cargo-xwin commands for check, clippy, and tests", () => {
    const check = parseArgs([], "/repo");
    expect(check.targetDir).toBe(path.join("/repo", ".gitpulse", "scratch", "target-win-cross"));
    expect(buildXwinArgs(check, "/repo")).toEqual([
      "xwin",
      "check",
      "--manifest-path",
      path.join("/repo", "src-tauri", "Cargo.toml"),
      "--locked",
      "--target",
      WINDOWS_TARGETS.x64,
      "--target-dir",
      path.join("/repo", ".gitpulse", "scratch", "target-win-cross"),
      "--all-targets",
    ]);

    const clippy = parseArgs(["--arch", "arm64", "--clippy"], "/repo");
    expect(buildXwinArgs(clippy, "/repo")).toContain(WINDOWS_TARGETS.arm64);
    expect(buildXwinArgs(clippy, "/repo").slice(-3)).toEqual(["--", "-D", "warnings"]);

    const tests = parseArgs(["--tests"], "/repo");
    expect(buildXwinArgs(tests, "/repo")).toContain("--no-run");
    expect(buildXwinArgs(tests, "/repo")).not.toContain("--all-targets");
  });

  it("fails closed with actionable diagnostics when target, cargo-xwin, or LLVM MSVC tools (clang-cl/lld-link/llvm-lib/llvm-rc) are missing", () => {
    const opts = parseArgs(["--arch", "x64"], "/repo");
    const missing = checkPrerequisites(
      opts,
      (program) => {
        if (program === "rustup") return { status: 0, stdout: "aarch64-apple-darwin\n" };
        // Simulate macOS with Xcode CLT `clang` only (no clang-cl/lld-link/llvm-lib/llvm-rc).
        if (program === "clang") return { status: 0, stdout: "Apple clang\n" };
        return enoent();
      },
      {},
    );
    expect(missing.ok).toBe(false);
    if (!missing.ok) {
      expect(missing.reasons.some((r) => r.includes("rustup target add x86_64-pc-windows-msvc"))).toBe(true);
      expect(missing.reasons.some((r) => r.includes("cargo install --locked cargo-xwin"))).toBe(true);
      const llvmReason = missing.reasons.find((r) => r.includes("Missing LLVM MSVC cross tools"));
      expect(llvmReason).toBeDefined();
      for (const tool of ["clang-cl", "lld-link", "llvm-lib", "llvm-rc"]) {
        expect(llvmReason).toContain(tool);
      }
      expect(llvmReason).toContain("/opt/homebrew/opt/llvm/bin");
    }

    const present = checkPrerequisites(
      opts,
      (program) => {
        if (program === "rustup") return { status: 0, stdout: "aarch64-apple-darwin\nx86_64-pc-windows-msvc\n" };
        return { status: 0, stdout: "ok\n" };
      },
      {},
    );
    expect(present).toEqual({ ok: true });
  });

  it("auto-discovers Homebrew keg-only LLVM and split LLD on macOS when missing from PATH", () => {
    const baseEnv = { PATH: "/usr/bin:/bin" };
    const arm64Llvm = HOMEBREW_LLVM_BIN_DIRS[0];
    const intelLlvm = HOMEBREW_LLVM_BIN_DIRS[1];
    const arm64Lld = HOMEBREW_LLD_BIN_DIRS[0];
    const intelLld = HOMEBREW_LLD_BIN_DIRS[1];

    // Homebrew LLVM 23+ splits `lld-link` into `/opt/homebrew/opt/lld/bin`.
    const arm64Split = resolveCrossEnv(
      baseEnv,
      "darwin",
      (p) => p === path.posix.join(arm64Llvm, "clang-cl") || p === path.posix.join(arm64Lld, "lld-link"),
    );
    expect(arm64Split.addedLlvmBin).toBe(arm64Llvm);
    expect(arm64Split.addedLldBin).toBe(arm64Lld);
    expect(arm64Split.addedBins).toEqual([arm64Llvm, arm64Lld]);
    expect(arm64Split.env.PATH).toBe(`${arm64Llvm}${path.posix.delimiter}${arm64Lld}${path.posix.delimiter}/usr/bin:/bin`);

    // Older Homebrew LLVM bundles `lld-link` inside `/usr/local/opt/llvm/bin`.
    const intelBundled = resolveCrossEnv(
      baseEnv,
      "darwin",
      (p) => p === path.posix.join(intelLlvm, "clang-cl") || p === path.posix.join(intelLlvm, "lld-link"),
    );
    expect(intelBundled.addedLlvmBin).toBe(intelLlvm);
    expect(intelBundled.addedLldBin).toBeNull();
    expect(intelBundled.addedBins).toEqual([intelLlvm]);
    expect(intelBundled.env.PATH).toBe(`${intelLlvm}${path.posix.delimiter}/usr/bin:/bin`);

    // Only `lld-link` missing (e.g., llvm already on PATH under minimal non-login PATH).
    const lldOnly = resolveCrossEnv(
      { PATH: `${intelLlvm}:/usr/bin` },
      "darwin",
      (p) =>
        p === path.posix.join(intelLlvm, "clang-cl") ||
        p === path.posix.join(intelLlvm, "llvm-lib") ||
        p === path.posix.join(intelLlvm, "llvm-rc") ||
        p === path.posix.join(intelLld, "lld-link"),
    );
    expect(lldOnly.addedLlvmBin).toBeNull();
    expect(lldOnly.addedLldBin).toBe(intelLld);
    expect(lldOnly.env.PATH).toBe(`${intelLld}${path.posix.delimiter}${intelLlvm}:/usr/bin`);

    // Untouched when all 4 tools are already on PATH.
    const alreadyOnPath = resolveCrossEnv(
      { PATH: "/custom/llvm/bin:/usr/bin" },
      "darwin",
      (p) => p.startsWith(path.posix.join("/custom/llvm/bin", "")),
    );
    expect(alreadyOnPath.addedLlvmBin).toBeNull();
    expect(alreadyOnPath.addedLldBin).toBeNull();
    expect(alreadyOnPath.addedBins).toEqual([]);
    expect(alreadyOnPath.env.PATH).toBe("/custom/llvm/bin:/usr/bin");

    // Untouched on Linux / Windows.
    const linux = resolveCrossEnv(baseEnv, "linux", () => true);
    expect(linux.addedLlvmBin).toBeNull();
    expect(linux.addedLldBin).toBeNull();
    expect(linux.addedBins).toEqual([]);
    expect(linux.env).toBe(baseEnv);
  });

  it("honors embed-resource RC_<target> / RC_<target_underscored> / RC precedence", () => {
    const target = WINDOWS_TARGETS.x64;
    expect(resolveRcCompiler(target, {})).toEqual({ program: "llvm-rc", envVar: null });
    expect(resolveRcCompiler(target, { RC: "/custom/rc" })).toEqual({ program: "/custom/rc", envVar: "RC" });
    expect(
      resolveRcCompiler(target, {
        RC: "/custom/rc",
        RC_x86_64_pc_windows_msvc: "/opt/windres",
      }),
    ).toEqual({ program: "/opt/windres", envVar: "RC_x86_64_pc_windows_msvc" });
    expect(
      resolveRcCompiler(target, {
        RC_x86_64_pc_windows_msvc: "/opt/windres",
        "RC_x86_64-pc-windows-msvc": "/opt/exact-rc",
      }),
    ).toEqual({ program: "/opt/exact-rc", envVar: "RC_x86_64-pc-windows-msvc" });

    const opts = parseArgs(["--arch", "x64"], "/repo");
    const customRcWorks = checkPrerequisites(
      opts,
      (program, args) => {
        if (program === "rustup") return { status: 0, stdout: `${target}\n` };
        if (program === "llvm-rc") return enoent();
        if (program === "/opt/windres") return { status: args[0] === "-V" ? 0 : 1, stdout: "GNU windres\n" };
        return { status: 0, stdout: "ok\n" };
      },
      { RC: "/opt/windres" },
    );
    expect(customRcWorks).toEqual({ ok: true });

    const brokenCustomRc = checkPrerequisites(
      opts,
      (program) => {
        if (program === "rustup") return { status: 0, stdout: `${target}\n` };
        if (program === "/missing/rc") return enoent();
        return { status: 0, stdout: "ok\n" };
      },
      { RC: "/missing/rc" },
    );
    expect(brokenCustomRc.ok).toBe(false);
    if (!brokenCustomRc.ok) {
      expect(brokenCustomRc.reasons.some((r) => r.includes("/missing/rc") && r.includes("RC"))).toBe(true);
    }
  });
});
