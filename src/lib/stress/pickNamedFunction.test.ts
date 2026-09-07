import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { pickNamedFunction } from "./pickNamedFunction";

const pulse = () => "pulse";
const storage = () => "storage";
const table = { PulseView: pulse, StoragePanel: storage };
const allowed = ["PulseView", "StoragePanel"] as const;

describe("pickNamedFunction", () => {
  it("returns the allowlisted own function", () => {
    expect(pickNamedFunction(table, "PulseView", allowed)).toBe(pulse);
    expect(pickNamedFunction(table, "PulseView", allowed)()).toBe("pulse");
  });

  it("refuses a name that is not on the allowlist even when the table has it", () => {
    const wider = { ...table, secret: () => "nope" };
    expect(() => pickNamedFunction(wider, "secret", allowed)).toThrow(/unsupported harness component/);
  });

  it("does not dispatch through Object.prototype", () => {
    expect(() => pickNamedFunction(table, "toString", ["toString"])).toThrow(
      /unsupported harness component/,
    );
    expect(() => pickNamedFunction(table, "constructor", ["constructor"])).toThrow(
      /unsupported harness component/,
    );
    expect(() => pickNamedFunction(table, "__proto__", ["__proto__"])).toThrow(
      /unsupported harness component/,
    );
  });

  it("refuses an allowlisted name whose own property is not a function", () => {
    const broken = { PulseView: "not a function" };
    expect(() => pickNamedFunction(broken, "PulseView", ["PulseView"])).toThrow(
      /unsupported harness component/,
    );
  });

  it("is the dispatch the stress harness uses, not a raw table lookup", () => {
    const src = readFileSync(fileURLToPath(new URL("../../../harness/stress.html", import.meta.url)), "utf8");
    expect(src).toContain("pickNamedFunction");
    expect(src).not.toMatch(/LOADERS\[COMPONENT\]\s*\(/);
  });
});
