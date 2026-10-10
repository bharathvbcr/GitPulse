import { globSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";
import { DEFERRED_UNDER_LOAD_MARKER, MAX_DEFERRED_RETRIES } from "../async/deferral";
import { withDeferralRetry } from "./invoke";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");

const DEFERRAL = `git status${DEFERRED_UNDER_LOAD_MARKER}2.013s: the git spawn rate limit admitted nothing sooner`;

function harness(
  answers: Array<unknown | Error | string>,
  extra: { random?: () => number; whenShown?: () => Promise<void> } = {},
) {
  const calls: Array<[string, unknown]> = [];
  const delays: number[] = [];
  let release: (() => void)[] = [];
  const raw = vi.fn(async (cmd: string, args?: unknown) => {
    calls.push([cmd, args]);
    const next = answers.length > 1 ? answers.shift() : answers[0];
    // Tauri rejects with the Rust `Err(String)` itself, not an Error.
    if (typeof next === "string" && next.includes(DEFERRED_UNDER_LOAD_MARKER)) throw next;
    if (next instanceof Error) throw next;
    return next;
  });
  const sleep = (ms: number) => {
    delays.push(ms);
    return new Promise<void>((resolve) => release.push(resolve));
  };
  // No jitter unless a test asks for it, so the schedule assertions below
  // pin the backoff itself.
  const invoke = withDeferralRetry(raw as never, {
    sleep,
    random: extra.random ?? (() => 0),
    whenShown: extra.whenShown ?? (() => Promise.resolve()),
  });
  const settle = async () => {
    for (let i = 0; i < 10; i += 1) await Promise.resolve();
  };
  /** Let pending awaits settle, then wake every sleeper once. */
  const tick = async () => {
    await settle();
    const wake = release;
    release = [];
    wake.forEach((resolve) => resolve());
    await settle();
  };
  return { invoke, raw, calls, delays, tick, settle, sleeping: () => release.length };
}

describe("withDeferralRetry", () => {
  it("asks again after the backoff, never straight back into the limit", async () => {
    const h = harness([DEFERRAL, "answer"]);
    const result = h.invoke<string>("cmd_list_branches", { repoPath: "/r" });
    await h.tick();
    await expect(result).resolves.toBe("answer");
    expect(h.raw).toHaveBeenCalledTimes(2);
    expect(h.delays).toEqual([3_000]);
  });

  it("does not retry a failure that is not a deferral", async () => {
    const h = harness([new Error("fatal: not a git repository")]);
    await expect(h.invoke("cmd_get_status", { repoPath: "/r" })).rejects.toThrow("not a git repository");
    expect(h.raw).toHaveBeenCalledTimes(1);
    expect(h.delays).toEqual([]);
  });

  it("gives the deferral back once the whole backoff is spent", async () => {
    const h = harness([DEFERRAL]);
    const result = h.invoke("cmd_get_status", { repoPath: "/r" });
    const settled = result.then(
      () => "resolved",
      (error: unknown) => error,
    );
    for (let i = 0; i <= MAX_DEFERRED_RETRIES; i += 1) await h.tick();
    expect(await settled).toBe(DEFERRAL);
    expect(h.raw).toHaveBeenCalledTimes(1 + MAX_DEFERRED_RETRIES);
    expect(h.delays).toEqual([3_000, 6_000, 12_000, 24_000, 30_000]);
  });

  it("joins an identical call to the retry already waiting", async () => {
    const h = harness([DEFERRAL, "answer"]);
    const first = h.invoke("cmd_stash_list", { repoPath: "/r" });
    await h.settle();
    expect(h.sleeping(), "first was declined and is waiting to retry").toBe(1);
    const second = h.invoke("cmd_stash_list", { repoPath: "/r" });
    const third = h.invoke("cmd_stash_list", { repoPath: "/r" });
    await h.tick();
    await expect(Promise.all([first, second, third])).resolves.toEqual(["answer", "answer", "answer"]);
    // One declined call plus one retry, however many callers asked meanwhile.
    expect(h.raw).toHaveBeenCalledTimes(2);
  });

  it("keeps calls with different arguments apart", async () => {
    const h = harness([DEFERRAL, DEFERRAL, "a", "b"]);
    const a = h.invoke("cmd_get_status", { repoPath: "/a" });
    const b = h.invoke("cmd_get_status", { repoPath: "/b" });
    await h.tick();
    await h.tick();
    await Promise.all([a, b]);
    expect(h.calls.filter(([, args]) => (args as { repoPath: string }).repoPath === "/a")).toHaveLength(2);
    expect(h.calls.filter(([, args]) => (args as { repoPath: string }).repoPath === "/b")).toHaveLength(2);
  });

  it("does not retry a call whose arguments are not plain data", async () => {
    class Channel {
      id = 7;
    }
    for (const args of [new Uint8Array([1, 2]), { onEvent: new Channel() }]) {
      const h = harness([DEFERRAL]);
      await expect(h.invoke("cmd_stream", args as never)).rejects.toBe(DEFERRAL);
      expect(h.raw).toHaveBeenCalledTimes(1);
    }
  });

  // Regression: every call one storm declined retried on the same schedule,
  // so they all met the same empty budget again, together, at 3 s, 6 s, ...
  it("spreads the retries of calls declined together, never earlier than the backoff", async () => {
    const draws = [0, 0.25, 0.5, 0.75, 0.999];
    const h = harness([DEFERRAL, DEFERRAL, DEFERRAL, DEFERRAL, DEFERRAL, "ok"], {
      random: () => draws.shift() ?? 0,
    });
    const calls = ["/a", "/b", "/c", "/d", "/e"].map((repoPath) =>
      h.invoke("cmd_get_status", { repoPath }),
    );
    await h.settle();
    expect(h.delays).toHaveLength(5);
    expect(new Set(h.delays).size, "five distinct arrival times").toBe(5);
    for (const delay of h.delays) {
      expect(delay).toBeGreaterThanOrEqual(3_000);
      expect(delay).toBeLessThanOrEqual(4_500);
    }
    await h.tick();
    await Promise.all(calls);
  });

  it("holds a retry while the window is hidden and runs it once shown", async () => {
    let show!: () => void;
    const shown = new Promise<void>((resolve) => {
      show = resolve;
    });
    const h = harness([DEFERRAL, "answer"], { whenShown: () => shown });
    const result = h.invoke<string>("cmd_get_status", { repoPath: "/r" });
    await h.tick();
    expect(h.raw, "the backoff elapsed, but nobody can see the answer").toHaveBeenCalledTimes(1);
    show();
    await h.settle();
    await expect(result).resolves.toBe("answer");
    expect(h.raw).toHaveBeenCalledTimes(2);
  });

  // A save or terminal write used to serialize its whole payload into a key
  // on every call, though the key only matters while a retry is waiting.
  it("does not serialize the arguments of a call nobody could join", async () => {
    const h = harness(["ok"]);
    let reads = 0;
    const args = {
      repoPath: "/r",
      get content() {
        reads += 1;
        return "x".repeat(1_000);
      },
    };
    await expect(h.invoke("cmd_write_file_content", args)).resolves.toBe("ok");
    expect(reads, "no retry was pending, so no key was computed").toBe(0);
  });

  it("computes the key when a call is deferred, so a later identical call still joins it", async () => {
    const h = harness([DEFERRAL, "answer"]);
    let reads = 0;
    const args = () => ({
      get repoPath() {
        reads += 1;
        return "/r";
      },
    });
    const first = h.invoke("cmd_stash_list", args());
    await h.settle();
    expect(h.sleeping(), "first was declined and is waiting to retry").toBe(1);
    expect(reads, "keyed once, at the deferral").toBe(1);
    const second = h.invoke("cmd_stash_list", args());
    await h.tick();
    await expect(Promise.all([first, second])).resolves.toEqual(["answer", "answer"]);
    expect(h.raw, "the identical call joined the waiting retry").toHaveBeenCalledTimes(2);
  });

  it("joins two identical calls that are both deferred before either is keyed", async () => {
    const h = harness([DEFERRAL, DEFERRAL, "answer"]);
    const first = h.invoke("cmd_stash_list", { repoPath: "/r" });
    const second = h.invoke("cmd_stash_list", { repoPath: "/r" });
    await h.settle();
    expect(h.sleeping(), "one retry chain for both").toBe(1);
    await h.tick();
    await expect(Promise.all([first, second])).resolves.toEqual(["answer", "answer"]);
    expect(h.raw).toHaveBeenCalledTimes(3);
  });

  it("gives back the deferral, not a serializer error, for arguments that cannot be keyed", async () => {
    const h = harness([DEFERRAL]);
    const cyclic: Record<string, unknown> = { repoPath: "/r" };
    cyclic.self = cyclic;
    await expect(h.invoke("cmd_stream", cyclic as never)).rejects.toBe(DEFERRAL);
    expect(h.raw).toHaveBeenCalledTimes(1);
  });

  it("starts a fresh chain once the previous one has settled", async () => {
    const h = harness([DEFERRAL, "one", DEFERRAL, "two"]);
    const first = h.invoke("cmd_last_fetch_at", { repoPath: "/r" });
    await h.tick();
    await expect(first).resolves.toBe("one");
    const second = h.invoke("cmd_last_fetch_at", { repoPath: "/r" });
    await h.tick();
    await expect(second).resolves.toBe("two");
    expect(h.raw).toHaveBeenCalledTimes(4);
  });
});

describe("the frontend has one IPC entry point", () => {
  it("no module outside src/lib/ipc imports invoke from Tauri directly", () => {
    const files = globSync("src/**/*.{ts,svelte}", { cwd: repoRoot }).map((file) =>
      file.split("\\").join("/"),
    );
    expect(files.length, "the walk found the source tree").toBeGreaterThan(100);
    const direct = files.filter((file) => {
      if (file.startsWith("src/lib/ipc/")) return false;
      if (/\.test\.ts$/.test(file) || file.includes("__tests__")) return false;
      return /import\s*\{[^}]*\binvoke\b[^}]*\}\s*from\s*["']@tauri-apps\/api\/core["']/.test(
        readFileSync(join(repoRoot, file), "utf8"),
      );
    });
    expect(direct, "route these through src/lib/ipc/invoke.ts").toEqual([]);
  });

  it("relies on a backend guarantee that is still in the Rust source", () => {
    const source = readFileSync(join(repoRoot, "src-tauri", "src", "engine", "git_cli.rs"), "utf8");
    // The retry above is safe only because a command that reached the
    // mutation guard never carries the deferral marker out.
    expect(/pub\(crate\) fn run_command_scope</.test(source), "run_command_scope exists").toBe(true);
    expect(/fn not_retryable\(/.test(source), "not_retryable exists").toBe(true);
  });
});
