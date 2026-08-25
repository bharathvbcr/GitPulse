import { describe, expect, it } from "vitest";
import { formatError } from "./formatError";

describe("formatError", () => {
  it("passes non-empty strings through trimmed", () => {
    expect(formatError("clone failed")).toBe("clone failed");
    expect(formatError("  clone failed \n")).toBe("clone failed");
  });

  it("maps blank strings to the unknown fallback", () => {
    expect(formatError("")).toBe("Unknown error");
    expect(formatError("   ")).toBe("Unknown error");
  });

  it("prefers the message field of Error instances", () => {
    expect(formatError(new Error("db locked"))).toBe("db locked");
    expect(formatError(new TypeError("not a function"))).toBe("not a function");
  });

  it("prefers a string .message on plain objects", () => {
    expect(formatError({ message: "rpc failed", code: -1 })).toBe("rpc failed");
    expect(formatError({ message: "  spaced  " })).toBe("spaced");
  });

  it("falls back to stable JSON when .message is absent or blank", () => {
    expect(formatError({ code: -1, kind: "io" })).toBe('{"code":-1,"kind":"io"}');
    expect(formatError({ b: 2, a: 1 })).toBe(formatError({ a: 1, b: 2 }));
    expect(formatError({ message: "", code: 7 })).toBe('{"code":7,"message":""}');
  });

  it("stringifies nested structures with sorted keys", () => {
    const err = { outer: { z: 1, a: [1, { y: 2, x: 3 }] } };
    expect(formatError(err)).toBe(
      '{"outer":{"a":[1,{"x":3,"y":2}],"z":1}}',
    );
  });

  it("serializes Map, Set, and BigInt payloads instead of throwing", () => {
    expect(formatError(new Map([["k", "v"]]))).toBe('{"k":"v"}');
    expect(formatError(new Set([1, 2]))).toBe("[1,2]");
    expect(formatError({ id: 9007199254740993n })).toBe(
      '{"id":"9007199254740993"}',
    );
  });

  it("renders nested Errors through their name and message", () => {
    expect(formatError({ cause: new Error("root") })).toBe(
      '{"cause":"Error: root"}',
    );
  });

  it("survives circular references without throwing", () => {
    const cyclic: Record<string, unknown> = { a: 1 };
    cyclic.self = cyclic;
    expect(formatError(cyclic)).toBe("Unknown error");
  });

  it("survives getters that throw during serialization", () => {
    const hostile = {
      get boom(): never {
        throw new Error("getter exploded");
      },
    };
    expect(formatError(hostile)).toBe("Unknown error");
  });

  it("maps undefined and null to the unknown fallback", () => {
    expect(formatError(undefined)).toBe("Unknown error");
    expect(formatError(null)).toBe("Unknown error");
  });

  it("renders primitives via String()", () => {
    expect(formatError(42)).toBe("42");
    expect(formatError(true)).toBe("true");
    expect(formatError(9007199254740993n)).toBe("9007199254740993");
    expect(formatError(Symbol("rate limited"))).toBe("Symbol(rate limited)");
  });

  it("maps exotic non-object values (functions) to the fallback", () => {
    expect(formatError(() => "nope")).toBe("Unknown error");
  });
});
