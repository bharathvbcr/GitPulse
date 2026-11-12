import { describe, expect, it } from "vitest";
import { forbiddenSymbols } from "./check-ptyd-release.mjs";

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
