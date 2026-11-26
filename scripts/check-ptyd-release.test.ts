import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { defaultBinaryPath, forbiddenSymbols, inspectBinary, main, parseArgs, symbolLines } from "./check-ptyd-release.mjs";

describe("check-ptyd-release forbiddenSymbols", () => {
  it("names the app-runtime symbols nm lists, and only those", () => {
    const nm = [
      "0000000100035490 t __RNvNtCs5vTnV24ikGc_12gitpulse_lib8terminal13start_session",
      "                 U __RNvXs_NtNtCshGgYbM4ce9H_12gitpulse_lib9workbench12terminal_runINtB4_11RunObserverINtCs62jSHxsxRZt_17tauri_runtime_wry3WryE",
      "0000000100042ca4 T __RNvCs62jSHxsxRZt_17tauri_runtime_wry4init",
      "0000000100050000 t _main",
      "",
    ].join("\n");
    expect(forbiddenSymbols(nm)).toEqual([
      "__RNvXs_NtNtCshGgYbM4ce9H_12gitpulse_lib9workbench12terminal_runINtB4_11RunObserverINtCs62jSHxsxRZt_17tauri_runtime_wry3WryE",
      "__RNvCs62jSHxsxRZt_17tauri_runtime_wry4init",
    ]);
  });

  it("finds nothing in a host that carries only its own engine", () => {
    expect(forbiddenSymbols("0000000100035490 t __RNvNtCs5vTnV24ikGc_12gitpulse_lib8terminal13start_session\n")).toEqual([]);
  });
});

describe("check-ptyd-release CLI and binary inspection", () => {
  it("parses --help, --binary, and rejects unknown flags", () => {
    expect(parseArgs(["--help"])).toEqual({ ok: true, help: true, binary: null });
    const parsed = parseArgs(["--binary", "/tmp/gitpulse-ptyd"]);
    expect(parsed).toEqual({ ok: true, help: false, binary: path.resolve("/tmp/gitpulse-ptyd") });
    expect(parseArgs(["--binary"]).ok).toBe(false);
    expect(parseArgs(["--unknown"]).ok).toBe(false);
    expect(main(["--help"])).toBe(0);
    expect(main(["--unknown"])).toBe(2);
  });

  it("respects CARGO_TARGET_DIR when locating the default binary", () => {
    const resolved = defaultBinaryPath({ CARGO_TARGET_DIR: "/tmp/custom-target" });
    expect(resolved).toBe(path.join(path.resolve("/tmp/custom-target"), "release", "gitpulse-ptyd"));
  });

  it("fails closed with exit 2 on missing binary or nm failure, 1 on leaked symbols, 0 when clean", () => {
    const dir = mkdtempSync(path.join(tmpdir(), "gitpulse-ptyd-check-"));
    try {
      const missing = path.join(dir, "missing-ptyd");
      expect(inspectBinary(missing).code).toBe(2);

      const bin = path.join(dir, "gitpulse-ptyd");
      writeFileSync(bin, "stub");
      expect(inspectBinary(bin, () => ({ status: 0, stdout: "0001 T _main\n", stderr: "" })).code).toBe(0);
      const multiArchClean = [
        "",
        "/tmp/gitpulse-ptyd (for architecture x86_64):",
        "0001 T _main",
        "",
        "/tmp/gitpulse-ptyd (for architecture arm64):",
        "0001 T _main",
        "",
      ].join("\n");
      expect(symbolLines(multiArchClean)).toEqual(["0001 T _main", "0001 T _main"]);
      expect(inspectBinary(bin, () => ({ status: 0, stdout: multiArchClean, stderr: "" })).code).toBe(0);
      const leaked = inspectBinary(bin, () => ({ status: 0, stdout: "0001 U RunObserver\n", stderr: "" }));
      expect(leaked.code).toBe(1);
      expect(leaked.offenders).toEqual(["RunObserver"]);
      expect(inspectBinary(bin, () => ({ status: 1, stdout: "", stderr: "bad binary" })).code).toBe(2);
      expect(inspectBinary(bin, () => ({ status: 0, stdout: "", stderr: "nm: no symbols" })).code).toBe(2);
      expect(inspectBinary(bin, () => ({ status: 0, stdout: "/tmp/gitpulse-ptyd (for architecture arm64):\n\n", stderr: "" })).code).toBe(2);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
