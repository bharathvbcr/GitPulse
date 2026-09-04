import { describe, expect, it } from "vitest";
import {
  DIAGNOSTICS_BUTTON_MODES,
  isDiagnosticsButtonMode,
  showsDiagnosticsButton,
} from "./diagnosticsButton";

describe("isDiagnosticsButtonMode", () => {
  it("accepts each mode and rejects everything else", () => {
    for (const mode of DIAGNOSTICS_BUTTON_MODES) {
      expect(isDiagnosticsButtonMode(mode)).toBe(true);
    }
    for (const value of ["", "errors", "Always", 0, null, undefined, {}]) {
      expect(isDiagnosticsButtonMode(value), `value: ${JSON.stringify(value)}`).toBe(false);
    }
  });
});

describe("showsDiagnosticsButton", () => {
  it("always shows the button under the default mode", () => {
    expect(showsDiagnosticsButton("always", 0)).toBe(true);
    expect(showsDiagnosticsButton("always", 5)).toBe(true);
  });

  it("hides it under 'issues' only while nothing at all is recorded", () => {
    // The condition is deliberately "the log is empty", not "no errors": a
    // warning-only log still has something to show, and a button that hid
    // recorded warnings would make an unexamined app look like a clean one.
    expect(showsDiagnosticsButton("issues", 0)).toBe(false);
    expect(showsDiagnosticsButton("issues", 1)).toBe(true);
  });
});
