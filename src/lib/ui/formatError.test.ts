import { afterEach, describe, expect, it, vi } from "vitest";
import {
  INTERNAL_ERROR_COPY,
  formatError,
  formatErrorRaw,
  isMissingFileError,
  isProgrammingError,
  markProgrammingErrorRecorded,
  setProgrammingErrorSink,
} from "./formatError";

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
    expect(formatErrorRaw(new TypeError("not a function"))).toBe("not a function");
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

  it("survives a throwing message getter before serialization", () => {
    const hostile = {
      get message(): never {
        throw new Error("message getter exploded");
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

describe("isMissingFileError", () => {
  it("matches the backend File not found signal anywhere in the formatted string", () => {
    expect(isMissingFileError("File not found: __main__.py")).toBe(true);
    expect(isMissingFileError("file not found: foo*.py")).toBe(true);
    expect(
      isMissingFileError("cmd_get_file_content failed: File not found: pkg/__main__.py"),
    ).toBe(true);
    expect(isMissingFileError(formatError({ message: "File not found: x" }))).toBe(true);
  });

  it("does not treat unrelated not-found phrasing as a missing coverage file", () => {
    expect(isMissingFileError("Unknown error")).toBe(false);
    // "profile not found:" contains the letters "file not found:" as a
    // substring; a bare includes() would swallow it.
    expect(isMissingFileError("profile not found:")).toBe(false);
    expect(isMissingFileError("not found: file")).toBe(false);
    expect(isMissingFileError("")).toBe(false);
  });
});

describe("isProgrammingError", () => {
  it("is true for the four JavaScript runtime faults", () => {
    for (const fault of [new TypeError("a"), new ReferenceError("b"), new RangeError("c"), new SyntaxError("d")]) {
      expect(isProgrammingError(fault)).toBe(true);
    }
  });

  it("is false for backend text, plain errors and objects", () => {
    for (const other of ["Cannot read properties of undefined", new Error("db locked"), { message: "rpc failed" }, null, undefined, 42]) {
      expect(isProgrammingError(other)).toBe(false);
    }
  });
});

describe("formatError on a JavaScript runtime fault", () => {
  const RAW = "Cannot read properties of undefined (reading 'invoke')";

  afterEach(() => {
    setProgrammingErrorSink(null);
    vi.restoreAllMocks();
  });

  it("returns the plain sentence and hands the raw error to the registered sink", () => {
    const sink = vi.fn();
    setProgrammingErrorSink(sink);
    for (const fault of [new TypeError(RAW), new ReferenceError("x is not defined"), new RangeError("Invalid array length"), new SyntaxError("Unexpected token")]) {
      sink.mockClear();
      const text = formatError(fault);
      expect(text).toBe(INTERNAL_ERROR_COPY);
      expect(text).not.toContain(fault.message);
      expect(sink).toHaveBeenCalledTimes(1);
      expect(sink).toHaveBeenCalledWith("ui", fault);
    }
  });

  it("names the caller's source when it gives one", () => {
    const sink = vi.fn();
    setProgrammingErrorSink(sink);
    const fault = new TypeError(RAW);
    formatError(fault, "storage");
    expect(sink).toHaveBeenCalledWith("storage", fault);
  });

  it("leaves strings, plain errors, IpcError-like and plain objects unchanged and records nothing", () => {
    const sink = vi.fn();
    setProgrammingErrorSink(sink);
    class IpcLike extends Error {
      constructor(readonly code: string, message: string) {
        super(message);
        this.name = "IpcError";
      }
    }
    expect(formatError("clone failed")).toBe("clone failed");
    expect(formatError(new Error("db locked"))).toBe("db locked");
    expect(formatError(new IpcLike("unclassified", "File not found: a.ts"))).toBe("File not found: a.ts");
    expect(formatError({ message: "rpc failed", code: -1 })).toBe("rpc failed");
    expect(formatError({ code: -1, kind: "io" })).toBe('{"code":-1,"kind":"io"}');
    expect(sink).not.toHaveBeenCalled();
  });

  it("falls back to console.error when no sink is registered, so the fault is never dropped", () => {
    setProgrammingErrorSink(null);
    const spy = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const fault = new TypeError(RAW);
    expect(formatError(fault)).toBe(INTERNAL_ERROR_COPY);
    expect(spy).toHaveBeenCalledTimes(1);
    expect(spy.mock.calls[0]).toContain(fault);
  });

  it("records one fault once, however often it is rendered", () => {
    const sink = vi.fn();
    setProgrammingErrorSink(sink);
    const fault = new TypeError(RAW);
    formatError(fault);
    formatError(fault);
    formatError(fault, "other");
    expect(sink).toHaveBeenCalledTimes(1);
  });

  it("does not record a fault that was already recorded elsewhere", () => {
    const sink = vi.fn();
    setProgrammingErrorSink(sink);
    const fault = new TypeError(RAW);
    markProgrammingErrorRecorded(fault);
    expect(formatError(fault)).toBe(INTERNAL_ERROR_COPY);
    expect(sink).not.toHaveBeenCalled();
  });

  it("formatErrorRaw keeps the fault's own text and records nothing", () => {
    const sink = vi.fn();
    setProgrammingErrorSink(sink);
    expect(formatErrorRaw(new TypeError(RAW))).toBe(RAW);
    expect(formatErrorRaw("clone failed")).toBe("clone failed");
    expect(sink).not.toHaveBeenCalled();
  });
});
