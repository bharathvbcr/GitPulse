import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { formatError } from "../ui/formatError";

const rejection = vi.hoisted(() => ({ payload: undefined as unknown }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => {
    throw rejection.payload;
  }),
}));

import { IPC_ERROR_KINDS, IpcError, invoke, ipcErrorKind, readIpcError } from "./invoke";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");

/**
 * One payload per kind, as `src-tauri/src/ipc_error/tests.rs` asserts Rust
 * serialises it. This reads each one back through the frontend's guard.
 */
const WIRE_CASES = JSON.parse(
  readFileSync(join(repoRoot, "src-tauri", "src", "ipc_error", "wire_cases.json"), "utf8"),
) as Array<{ kind: string; message: string; detail?: unknown }>;

/**
 * What the entry point throws when the command rejects with `payload`. A
 * deferral is not asked of it here: that kind is retried on a seconds-long
 * backoff, which invoke.test.ts covers with an injected clock.
 */
async function rejectedWith(payload: unknown): Promise<unknown> {
  rejection.payload = payload;
  return invoke("cmd_anything").then(
    () => {
      throw new Error("the command was expected to reject");
    },
    (error: unknown) => error,
  );
}

describe("every kind Rust serialises is read back by the guard", () => {
  it("has a wire case for each kind in the list, and no other", () => {
    expect(WIRE_CASES.map((c) => c.kind).sort()).toEqual([...IPC_ERROR_KINDS].sort());
  });

  it.each(WIRE_CASES)("$kind", async (wire) => {
    const read = readIpcError(wire);
    expect(read).toBeInstanceOf(IpcError);
    expect(read?.kind).toBe(wire.kind);
    expect(read?.message).toBe(wire.message);
    expect(read?.detail).toEqual(wire.detail);

    // The same through the entry point a component calls.
    if (wire.kind === "deferred_under_load") return;
    const thrown = await rejectedWith(wire);
    expect(thrown).toBeInstanceOf(IpcError);
    expect(ipcErrorKind(thrown)).toBe(wire.kind);
  });
});

describe("what is not an IpcError is opaque", () => {
  it.each([
    ["a string", "Refresh the stash list and try again."],
    ["an Error", new Error("Refresh the stash list and try again.")],
    ["a { code, message } payload", { code: "refused", message: "x" }],
    ["an unknown kind", { kind: "from_the_future", message: "x" }],
    ["a kind without a message", { kind: "stale_stash" }],
    ["null", null],
    ["a number", 7],
  ])("%s has no kind to branch on", (_name, payload) => {
    expect(readIpcError(payload)).toBeNull();
    expect(ipcErrorKind(payload)).toBe("unclassified");
  });

  it("an unclassified message is never read, whatever it says", () => {
    for (const wire of WIRE_CASES) {
      const opaque = readIpcError({ kind: "unclassified", message: wire.message });
      expect(ipcErrorKind(opaque), wire.message).toBe("unclassified");
    }
    expect(ipcErrorKind(WIRE_CASES.map((c) => c.message))).toBe("unclassified");
  });
});

describe("a command's error shows the message it always showed", () => {
  beforeEach(() => {
    rejection.payload = undefined;
  });

  it.each(WIRE_CASES.filter((c) => c.kind !== "deferred_under_load"))("$kind: formatError, String, a template and .message agree with the old string", async (wire) => {
    const thrown = await rejectedWith(wire);
    const old = wire.message;
    expect(formatError(thrown)).toBe(old.trim());
    expect(String(thrown)).toBe(old);
    expect(`${thrown as IpcError}`).toBe(old);
    expect((thrown as Error).message).toBe(old);
    // The `err instanceof Error ? err.message : String(err)` callers write.
    expect(thrown instanceof Error ? thrown.message : String(thrown)).toBe(old);
  });

  it("a rejection that is not an IpcError reaches the caller untouched", async () => {
    const plain = "not a Git repository";
    expect(await rejectedWith(plain)).toBe(plain);
    const typed = { code: "refused", message: "Refused" };
    expect(await rejectedWith(typed)).toBe(typed);
  });
});
