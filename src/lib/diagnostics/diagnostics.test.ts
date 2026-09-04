import { describe, expect, it, vi } from "vitest";
import { STRESS_TIMEOUT_MS, expectWithinBudget } from "../__tests__/perfBudget";
import { get } from "svelte/store";
import {
  APP_VERSION,
  DIAGNOSTIC_STORAGE_KEY,
  MAX_DIAGNOSTIC_ENTRIES,
  createDiagnostics,
  formatDiagnosticReport,
  formatDiagnosticTime,
  installGlobalDiagnostics,
  diagnosticFingerprint,
  isHostRuntimeNoise,
  redactDiagnosticText,
  isSecretFieldName,
  normalizeFieldName,
  type DiagnosticEntry,
  type DiagnosticSeverity,
} from "./diagnostics";
import { memoryStorage } from "../repos/persist";

function makeStore(now: number[] = [], storage = memoryStorage()) {
  let tick = 0;
  return {
    store: createDiagnostics({
      storage,
      now: () => now[tick++] ?? now[now.length - 1] ?? 0,
    }),
    storage,
  };
}

describe("createDiagnostics", () => {
  it("redacts credentials before they reach memory, localStorage, or reports", () => {
    const storage = memoryStorage();
    const store = createDiagnostics({ storage, now: () => 5 });
    const key = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";
    store.error(
      `auth-${key}`,
      `git push https://user:${key}@github.com/o/r\nAuthorization: Bearer ${key}`,
    );

    const [entry] = get(store);
    const persisted = storage.getItem(DIAGNOSTIC_STORAGE_KEY) ?? "";
    const report = formatDiagnosticReport([entry], new Date("2026-01-01T00:00:00Z"));
    for (const surface of [entry.source, entry.message, persisted, report]) {
      expect(surface).not.toContain(key);
    }
    expect(entry.source).toContain("ghp_");
    expect(persisted).toContain("ghp_");
    expect(report).toContain("ghp_");
  });

  it("redacts generic auth headers, cookies, URL passwords, named secrets, and private-key blocks", () => {
    const privateKey = [
      "-----BEGIN PRIVATE KEY-----",
      "super-secret-base64-body",
      "-----END PRIVATE KEY-----",
    ].join("\n");
    const out = redactDiagnosticText(
      `Authorization: Basic dXNlcjpwYXNz\nAuthorization: Digest opaque-digest-secret\nCookie: session=secret\nhttps://me:p4ss@example.test/r?access_token=query-secret\npostgres://dbuser:database-secret@example.test/app\n{"Authorization":"Token json-auth-secret"}\n{"Cookie":"session=json-cookie-secret"}\n{"api_key":"json-secret"}\n${privateKey}`,
    );
    expect(out).toContain("Authorization: Basic <redacted>");
    expect(out).toContain("Cookie: <redacted>");
    expect(out).toContain("https://me:<redacted>@example.test/r");
    expect(out).toContain("<private key redacted>");
    expect(out).not.toContain("dXNlcjpwYXNz");
    expect(out).not.toContain("opaque-digest-secret");
    expect(out).not.toContain("session=secret");
    expect(out).not.toContain("database-secret");
    expect(out).not.toContain("json-auth-secret");
    expect(out).not.toContain("json-cookie-secret");
    expect(out).not.toContain("query-secret");
    expect(out).not.toContain("json-secret");
    expect(out).toContain("access_token=<redacted>");
    expect(out).toContain("postgres://dbuser:<redacted>@example.test/app");
    expect(out).toContain('{"Authorization":"<redacted>"}');
    expect(out).toContain('{"Cookie":"<redacted>"}');
    expect(out).toContain('{"api_key":"<redacted>"}');
    expect(out).not.toContain("super-secret-base64-body");
    expect(redactDiagnosticText(out)).toBe(out);
  });

  it("redacts serialized and separate-value credentials without corrupting JSON", () => {
    const cases = [
      [
        '["git","-c","http.extraHeader=Authorization: Bearer opaque-auth","fetch"]',
        '["git","-c","http.extraHeader=Authorization: Bearer <redacted>","fetch"]',
        "opaque-auth",
      ],
      [
        '["curl","-H","Cookie: session=opaque-cookie","https://example.test"]',
        '["curl","-H","Cookie: <redacted>","https://example.test"]',
        "opaque-cookie",
      ],
      [
        '{"error":"password=opaque-password","phase":"gate"}',
        '{"error":"password=<redacted>","phase":"gate"}',
        "opaque-password",
      ],
      [
        '["tool","Authorization: Bearer opaque-escaped\\\\nnext","tail"]',
        '["tool","Authorization: Bearer <redacted>","tail"]',
        "opaque-escaped",
      ],
      [
        '["tool","AWS_SECRET_ACCESS_KEY=opaque-aws","next"]',
        '["tool","AWS_SECRET_ACCESS_KEY=<redacted>","next"]',
        "opaque-aws",
      ],
      [
        '["tool","-----BEGIN PGP PRIVATE KEY BLOCK-----\\\\nopaque-pgp\\\\n-----END PGP PRIVATE KEY BLOCK-----","next"]',
        '["tool","<private key redacted>","next"]',
        "opaque-pgp",
      ],
      [
        '["tool","-----BEGIN PRIVATE KEY-----\\\\nopaque-pem\\\\n-----END PRIVATE KEY-----","next"]',
        '["tool","<private key redacted>","next"]',
        "opaque-pem",
      ],
      [
        '["tool","--password","opaque-cli-password","next"]',
        '["tool","--password","<redacted>","next"]',
        "opaque-cli-password",
      ],
      [
        '["curl","--user","alice:opaque-basic-password","https://example.test"]',
        '["curl","--user","alice:<redacted>","https://example.test"]',
        "opaque-basic-password",
      ],
      [
        'argv=["tool","--password","opaque-wrapped-password","next"]',
        'argv=["tool","--password","<redacted>","next"]',
        "opaque-wrapped-password",
      ],
      [
        'prefix ["curl","--user","alice:opaque-wrapped-basic"] suffix',
        'prefix ["curl","--user","alice:<redacted>"] suffix',
        "opaque-wrapped-basic",
      ],
      [
        "command=tool --access-token opaque-shell-token next",
        "command=tool --access-token <redacted> next",
        "opaque-shell-token",
      ],
      [
        "command=curl --user alice:opaque-shell-password https://example.test",
        "command=curl --user <redacted> https://example.test",
        "opaque-shell-password",
      ],
      [
        'phase [broken argv=["tool","--password","opaque-after-broken-bracket","next"]',
        'phase [broken argv=["tool","--password","<redacted>","next"]',
        "opaque-after-broken-bracket",
      ],
      [
        '[unclosed prefix ["curl","--user","alice:opaque-after-unclosed","https://example.test"] suffix',
        '[unclosed prefix ["curl","--user","alice:<redacted>","https://example.test"] suffix',
        "opaque-after-unclosed",
      ],
      [
        '{"message":"argv=[\\"tool\\",\\"--password\\",\\"opaque-nested-password\\",\\"next\\"]"}',
        '{"message":"argv=[\\"tool\\",\\"--password\\",\\"<redacted>\\",\\"next\\"]"}',
        "opaque-nested-password",
      ],
    ] as const;

    for (const [input, expected, secret] of cases) {
      const out = redactDiagnosticText(input);
      expect(out).not.toContain(secret);
      expect(out).toBe(expected);
      try {
        JSON.parse(input);
        expect(() => JSON.parse(out)).not.toThrow();
      } catch {
        // Wrapper prose is intentionally not JSON; only complete JSON inputs
        // carry a structure-preservation assertion.
      }
      expect(redactDiagnosticText(out)).toBe(out);
    }
  });

  it("redacts recursively serialized strings without corrupting their outer JSON", () => {
    const cases = [
      ["opaque-depth-cli", JSON.stringify(["tool", "--password", "opaque-depth-cli", "next"])],
      ["opaque-depth-auth", "Authorization: Bearer opaque-depth-auth"],
      ["opaque-depth-cookie", "Cookie: session=opaque-depth-cookie"],
      ["opaque-depth-key", "api_key=opaque-depth-key"],
    ] as const;

    for (const [secret, payload] of cases) {
      const nested = JSON.stringify({ message: payload });
      const input = JSON.stringify({ message: nested });
      const out = redactDiagnosticText(input);
      expect(out).not.toContain(secret);
      const parsed = JSON.parse(out) as { message: string };
      const parsedNested = JSON.parse(parsed.message) as { message: string };
      if (payload.startsWith("[")) expect(() => JSON.parse(parsedNested.message)).not.toThrow();
      expect(redactDiagnosticText(out)).toBe(out);
    }
  });

  it("fails closed when structured JSON reaches the serialized nesting cap", () => {
    const secret = "opaque-boundary-auth";
    let payload: unknown = `Authorization: Bearer ${secret}`;
    for (let depth = 0; depth < 40; depth += 1) {
      payload = { message: payload };
    }
    const input = JSON.stringify(payload);

    const out = redactDiagnosticText(input);

    expect(out).not.toContain(secret);
    expect(() => JSON.parse(out)).not.toThrow();
    expect(redactDiagnosticText(out)).toBe(out);
  });

  it("records errors and warnings newest-first through formatError", () => {
    const { store } = makeStore([10, 20]);
    store.error("console", new TypeError("nope"));
    store.warn("console", "watch out");
    const entries = get(store);
    expect(entries.map((entry) => entry.severity)).toEqual(["warning", "error"]);
    expect(entries[1].message).toBe("nope");
    expect(entries.map((entry) => entry.id)).toEqual([2, 1]);
  });

  it("records a hostile thrown-message getter without rethrowing from diagnostics", () => {
    const { store } = makeStore([10]);
    const hostile = {
      get message(): string {
        throw new Error("getter exploded");
      },
    };

    expect(() => store.error("unhandled-rejection", hostile)).not.toThrow();
    expect(get(store)[0]?.message).toBe("Unknown error");
  });

  it("coalesces identical consecutive repeats into a counted entry", () => {
    const { store } = makeStore([1, 2, 3]);
    store.error("repo", "clone failed");
    store.error("repo", "clone failed");
    store.error("repo", "clone failed");
    const entries = get(store);
    expect(entries).toHaveLength(1);
    expect(entries[0].count).toBe(3);
    expect(entries[0].at).toBe(3);
  });

  it("keeps non-consecutive repeats as separate entries", () => {
    const { store } = makeStore();
    store.error("a", "same");
    store.warn("b", "other");
    store.error("a", "same");
    const entries = get(store);
    expect(entries).toHaveLength(3);
    expect(entries.every((entry) => entry.count === 1)).toBe(true);
  });

  it("drops the oldest entries past the ring-buffer bound", () => {
    const { store } = makeStore();
    for (let i = 0; i < MAX_DIAGNOSTIC_ENTRIES + 5; i += 1) {
      store.error("test", `failure ${i}`);
    }
    const entries = get(store);
    expect(entries).toHaveLength(MAX_DIAGNOSTIC_ENTRIES);
    expect(entries[0].message).toBe(`failure ${MAX_DIAGNOSTIC_ENTRIES + 4}`);
    expect(entries.at(-1)?.message).toBe("failure 5");
  });

  it("clamps oversized messages instead of storing them whole", () => {
    const { store } = makeStore();
    store.error("test", `START${"x".repeat(5000)}END`);
    const entries = get(store);
    expect(entries[0].message.length).toBeLessThanOrEqual(2000);
    // This used to assert the message ended in "…", which encoded the very
    // defect being fixed: truncating from the head threw the ending away, and
    // for command output the ending is where the reason lives. A clamped
    // message now keeps both ends and says what it dropped in between.
    expect(entries[0].message).toMatch(/… \d+ characters elided …/);
    expect(entries[0].message.startsWith("START")).toBe(true);
    expect(entries[0].message.endsWith("END")).toBe(true);
  });

  it("persists entries and restores them into a fresh instance", () => {
    const storage = memoryStorage();
    const first = createDiagnostics({ storage, now: () => 5 });
    first.error("pane-crash", "graph blew up");
    expect(JSON.parse(storage.getItem(DIAGNOSTIC_STORAGE_KEY) ?? "[]")).toHaveLength(1);

    const second = createDiagnostics({ storage, now: () => 6 });
    const entries = get(second);
    expect(entries.map((entry) => entry.source)).toEqual(["pane-crash"]);
    // New entries continue the restored id sequence instead of colliding.
    second.error("console", "fresh failure");
    const after = get(second);
    expect(after[0].id).toBeGreaterThan(entries[0].id);
  });

  it("survives corrupt or hostile persisted blobs", () => {
    const storage = memoryStorage({
      [DIAGNOSTIC_STORAGE_KEY]: JSON.stringify([
        null,
        42,
        { id: "x", at: 1, severity: "error", source: "s", message: "m" },
        { id: 7, at: Number.NaN, severity: "error", source: "s", message: "bad time" },
        { id: 9, at: 3, severity: "catastrophic", source: "s", message: "bad severity" },
        { id: 11, at: 4, severity: "warning", source: "ok", message: "kept" },
        { id: 5, at: 2, severity: "error", source: "reordered", message: "sorted by id", count: 0 },
      ]),
    });
    const store = createDiagnostics({ storage });
    const entries = get(store);
    expect(entries.map((entry) => entry.id)).toEqual([11, 5]);
    expect(entries[1].count).toBe(1);
  });

  it("starts empty when the blob is not valid JSON", () => {
    const storage = memoryStorage({ [DIAGNOSTIC_STORAGE_KEY]: "{not json" });
    const store = createDiagnostics({ storage });
    expect(get(store)).toEqual([]);
  });

  it("clear() empties memory and removes the persisted blob", () => {
    const { store, storage } = makeStore();
    store.error("test", "gone soon");
    expect(storage.getItem(DIAGNOSTIC_STORAGE_KEY)).not.toBeNull();
    store.clear();
    expect(get(store)).toEqual([]);
    expect(storage.getItem(DIAGNOSTIC_STORAGE_KEY)).toBeNull();
  });

  it("works with storage disabled entirely", () => {
    const store = createDiagnostics({ storage: null });
    store.error("test", "memory only");
    expect(get(store)).toHaveLength(1);
  });

  it("does not record host-runtime noise that is not a GitPulse failure", () => {
    const { store } = makeStore();
    store.warn(
      "console",
      "[TAURI] Couldn't find callback id 2063278846. This might happen when the app is reloaded while Rust is running an asynchronous operation.",
    );
    store.warn(
      "console",
      "IPC custom protocol failed, Tauri will now use the postMessage interface instead Load failed",
    );
    store.error(
      "console",
      "[hmr] Failed to reload /src/app.css. This could be due to syntax errors or importing non-existent modules. (see errors above)",
    );
    store.error("console", "Importing a module script failed.");
    store.error("unhandled-rejection", "undefined is not an object (evaluating 'module.default')");
    store.error("repo", "clone failed");
    expect(get(store).map((entry) => entry.message)).toEqual(["clone failed"]);
  });

  it("drops host-runtime noise when restoring a persisted blob", () => {
    const storage = memoryStorage({
      [DIAGNOSTIC_STORAGE_KEY]: JSON.stringify([
        {
          id: 2,
          at: 2,
          severity: "warning",
          source: "console",
          message:
            "[TAURI] Couldn't find callback id 1. This might happen when the app is reloaded while Rust is running an asynchronous operation.",
          count: 12,
        },
        {
          id: 1,
          at: 1,
          severity: "error",
          source: "pane-crash",
          message: "graph blew up",
          count: 1,
        },
      ]),
    });
    const store = createDiagnostics({ storage });
    expect(get(store).map((entry) => entry.message)).toEqual(["graph blew up"]);
    expect(JSON.parse(storage.getItem(DIAGNOSTIC_STORAGE_KEY) ?? "[]")).toEqual([
      { id: 1, at: 1, severity: "error", source: "pane-crash", message: "graph blew up", count: 1 },
    ]);
  });
});
describe("isHostRuntimeNoise", () => {
  it("matches the Tauri reload, IPC fallback, and Vite HMR messages from a WKWebView session", () => {
    const fromDump = [
      "[TAURI] Couldn't find callback id 3802601472. This might happen when the app is reloaded while Rust is running an asynchronous operation.",
      "IPC custom protocol failed, Tauri will now use the postMessage interface instead Load failed",
      "[hmr] Failed to reload /src/lib/components/SettingsModal.svelte. This could be due to syntax errors or importing non-existent modules. (see errors above)",
      "Importing a module script failed.",
      "undefined is not an object (evaluating 'module.default')",
      "ResizeObserver loop completed with undelivered notifications.",
      "ResizeObserver loop limit exceeded",
    ];
    for (const message of fromDump) {
      expect(isHostRuntimeNoise(message), message).toBe(true);
    }
  });

  it("does not swallow product failures that merely mention reload or modules", () => {
    const keep = [
      "clone failed",
      "Couldn't find callback in the rebase plan",
      "Failed to reload the repository graph",
      "Importing a patch failed.",
      "undefined is not an object (evaluating 'commit.defaultBranch')",
    ];
    for (const message of keep) {
      expect(isHostRuntimeNoise(message), message).toBe(false);
    }
  });
});

describe("formatDiagnosticReport", () => {
  const generatedAt = new Date("2026-08-25T12:00:00Z");

  it("reports an empty log explicitly", () => {
    expect(formatDiagnosticReport([], generatedAt)).toContain("nothing recorded");
  });

  it("sums coalesced occurrences and lists distinct entries", () => {
    const entries: DiagnosticEntry[] = [
      { id: 2, at: Date.parse("2026-08-25T11:00:00Z"), severity: "warning", source: "console", message: "careful", count: 2 },
      { id: 1, at: Date.parse("2026-08-25T10:00:00Z"), severity: "error", source: "repo", message: "boom", count: 3 },
    ];
    const report = formatDiagnosticReport(entries, generatedAt);
    expect(report).toContain("3 error(s), 2 warning(s), 2 distinct");
    expect(report).toContain("[2026-08-25T11:00:00.000Z] WARNING x2 (console)");
    expect(report).toContain("[2026-08-25T10:00:00.000Z] ERROR x3 (repo)");
    expect(report).toContain("Generated: 2026-08-25T12:00:00.000Z");
  });

  it("indents multi-line messages so they stay inside one block", () => {
    const entries: DiagnosticEntry[] = [
      { id: 1, at: 0, severity: "error", source: "t", message: "line one\nline two", count: 1, version: "1.2.3" },
    ];
    // Recorded by the running build, so the header carries no build note.
    const report = formatDiagnosticReport(entries, generatedAt, "1.2.3");
    expect(report).toContain("(t)\n  line one\n  line two");
  });

  it("marks single occurrences without an x-count suffix", () => {
    const entries: DiagnosticEntry[] = [
      { id: 1, at: 0, severity: "error", source: "t", message: "once", count: 1 },
    ];
    expect(formatDiagnosticReport(entries, generatedAt)).toContain("] ERROR (t)");
  });
});

describe("formatDiagnosticTime", () => {
  it("shows only the clock for today and prepends the date otherwise", () => {
    const now = new Date(2026, 7, 25, 12, 0, 0).getTime();
    const today = new Date(2026, 7, 25, 9, 30, 5).getTime();
    const earlier = new Date(2026, 6, 1, 9, 30, 5).getTime();

    const todayText = formatDiagnosticTime(today, now);
    const earlierText = formatDiagnosticTime(earlier, now);
    expect(todayText).toMatch(/^\d{2}:\d{2}:\d{2}$/);
    expect(earlierText).toContain("2026");
    expect(earlierText).not.toBe(todayText);
  });
});

describe("installGlobalDiagnostics", () => {
  type FailureEvent = { reason?: unknown; error?: unknown; message?: unknown };
  interface FakeTarget {
    addEventListener(type: string, listener: (event: FailureEvent) => void): void;
    removeEventListener(type: string, listener: (event: FailureEvent) => void): void;
    emit(type: string, event: FailureEvent): void;
  }

  function makeTarget(): FakeTarget {
    const listeners = new Map<string, Array<(event: FailureEvent) => void>>();
    return {
      addEventListener(type, listener) {
        listeners.set(type, [...(listeners.get(type) ?? []), listener]);
      },
      removeEventListener(type, listener) {
        listeners.set(
          type,
          (listeners.get(type) ?? []).filter((candidate) => candidate !== listener),
        );
      },
      emit(type, event) {
        for (const listener of listeners.get(type) ?? []) listener(event);
      },
    };
  }

  function setup() {
    const recorded: Array<{ severity: DiagnosticSeverity; source: string; message: string }> = [];
    const sink = {
      error: (source: string, detail: unknown) =>
        recorded.push({ severity: "error" as const, source, message: String(detail) }),
      warn: (source: string, detail: unknown) =>
        recorded.push({ severity: "warning" as const, source, message: String(detail) }),
    };
    // The installer replaces con.error/con.warn with wrappers, so tests keep
    // references to the original spies to observe forwarded calls.
    const originalError = vi.fn();
    const originalWarn = vi.fn();
    const con = { error: originalError, warn: originalWarn };
    const target = makeTarget();
    const uninstall = installGlobalDiagnostics(sink, { target, console: con });
    return { recorded, originalError, originalWarn, con, target, uninstall };
  }

  it("records console.error/warn calls and still forwards them untouched", () => {
    const { recorded, originalError, originalWarn, con, uninstall } = setup();
    con.error("disk full", { code: 28 });
    con.warn("getting tight");
    expect(recorded).toEqual([
      { severity: "error", source: "console", message: 'disk full {"code":28}' },
      { severity: "warning", source: "console", message: "getting tight" },
    ]);
    expect(originalError).toHaveBeenCalledWith("disk full", { code: 28 });
    expect(originalWarn).toHaveBeenCalledWith("getting tight");
    uninstall();
  });

  it("records unhandled rejections and uncaught errors from window events", () => {
    const { recorded, target, uninstall } = setup();
    target.emit("unhandledrejection", { reason: new Error("promise died") });
    target.emit("error", { error: undefined, message: "syntax goop" });
    target.emit("error", { error: new TypeError("typed badly") });
    expect(recorded.map((entry) => entry.source)).toEqual([
      "unhandled-rejection",
      "uncaught-error",
      "uncaught-error",
    ]);
    expect(recorded[0].message).toContain("promise died");
    expect(recorded[1].message).toBe("syntax goop");
    expect(recorded[2].message).toContain("typed badly");
    uninstall();
  });

  it("forwards host-runtime noise to the original console without recording it", () => {
    const { recorded, originalWarn, originalError, con, target, uninstall } = setup();
    con.warn(
      "[TAURI] Couldn't find callback id 1. This might happen when the app is reloaded while Rust is running an asynchronous operation.",
    );
    con.error("Importing a module script failed.");
    target.emit("unhandledrejection", {
      reason: "undefined is not an object (evaluating 'module.default')",
    });
    expect(recorded).toEqual([]);
    expect(originalWarn).toHaveBeenCalledTimes(1);
    expect(originalError).toHaveBeenCalled();
    uninstall();
  });

  it("keeps the legacy console prefixes on global failures", () => {
    const { originalError, target, uninstall } = setup();
    target.emit("unhandledrejection", { reason: "late" });
    target.emit("error", { message: "sync death" });
    expect(originalError).toHaveBeenNthCalledWith(
      1,
      "[gitpulse] unhandled promise rejection: late",
    );
    expect(originalError).toHaveBeenNthCalledWith(2, "[gitpulse] uncaught error: sync death");
    uninstall();
  });

  it("does not recurse when the sink itself logs via console", () => {
    const originalError = vi.fn();
    const con = { error: originalError, warn: vi.fn() };
    const sink = {
      error: (_source: string, _detail: unknown) => {
        con.error("sink exploded");
      },
      warn: () => {},
    };
    installGlobalDiagnostics(sink, { target: makeTarget(), console: con });
    expect(() => con.error("original")).not.toThrow();
    // Inner call first (during recording), then the forwarded original.
    expect(originalError).toHaveBeenNthCalledWith(1, "sink exploded");
    expect(originalError).toHaveBeenNthCalledWith(2, "original");
  });

  it("restores the original console functions and detaches listeners", () => {
    const { recorded, originalError, con, target, uninstall } = setup();
    const wrapper = con.error;
    uninstall();
    uninstall(); // double-uninstall stays safe
    expect(con.error).not.toBe(wrapper);
    con.error("after restore");
    target.emit("unhandledrejection", { reason: "late again" });
    expect(recorded).toEqual([]);
    expect(originalError).toHaveBeenCalledTimes(1);
  });
});

/**
 * The ring is what a user copies to report a problem, so a clamped entry must
 * keep the part that explains the failure.
 *
 * Head-only truncation kept the wrong half: a real coverage failure produced
 * an 18,580-character message whose cause sat at character 18,356, and the
 * 2,000-character head held nothing but the aborting script's own chatter.
 */
describe("message clamping keeps both ends", () => {
  function longMessage(): string {
    const header = 'Coverage command "pytest --cov" failed (exit 3):';
    const noise = Array.from({ length: 900 }, (_, i) => `  ok   check number ${i}`).join("\n");
    const cause = [
      'INTERNALERROR>   File "/repo/bench/stress_test.py", line 944, in <module>',
      "INTERNALERROR>     sys.exit(1 if FAIL else 0)",
      "INTERNALERROR> SystemExit: 0",
      "============================ no tests ran in 17.74s ============================",
    ].join("\n");
    return [header, noise, cause].join("\n");
  }

  function recordAndRead(message: string) {
    const store = createDiagnostics({ storage: null, now: () => 0 });
    store.error("coverage", message);
    let entries: readonly { message: string }[] = [];
    store.subscribe((next) => (entries = next))();
    return entries[0].message;
  }

  it("keeps the ending, where the reason for a failure lives", () => {
    const kept = recordAndRead(longMessage());
    expect(kept).toContain("stress_test.py");
    expect(kept).toContain("line 944");
    expect(kept).toContain("SystemExit: 0");
    expect(kept).toContain("no tests ran");
  });

  it("keeps the beginning, which names the command", () => {
    const kept = recordAndRead(longMessage());
    expect(kept).toContain('Coverage command "pytest --cov" failed (exit 3):');
  });

  it("announces the elision, and the accounting is exact", () => {
    const message = longMessage();
    const kept = recordAndRead(message);
    const notice = kept.match(/\n… (\d+) characters elided …\n/);
    expect(notice, "a clamped message must say what it dropped").not.toBeNull();
    const dropped = Number(notice![1]);
    const retained = kept.length - notice![0].length;
    // Nothing unaccounted for: what was kept plus what was dropped is the
    // whole original message.
    expect(retained + dropped).toBe(message.length);
  });

  it("pays for the elision notice out of the budget, not on top of it", () => {
    const kept = recordAndRead(longMessage());
    expect(kept.length).toBeLessThanOrEqual(2000);
  });

  it("leaves a message inside the budget completely untouched", () => {
    const short = "Coverage command failed (exit 1):\nboom";
    expect(recordAndRead(short)).toBe(short);
    const exact = "x".repeat(2000);
    expect(recordAndRead(exact)).toBe(exact);
    expect(recordAndRead("x".repeat(2001))).toContain("characters elided");
  });

  it("never loses both ends of a pathological single-line payload", () => {
    const kept = recordAndRead(`START${"y".repeat(50_000)}END`);
    expect(kept.startsWith("START")).toBe(true);
    expect(kept.endsWith("END")).toBe(true);
  }, STRESS_TIMEOUT_MS);
});

/**
 * The ring is persisted, so it outlives the build that wrote it. Without a
 * stamp, a log copied after an upgrade presents entries from an older build
 * as though they described the running one — which is how an already-fixed
 * bug reads as a live one.
 */
describe("build stamping", () => {
  const generatedAt = new Date("2026-08-25T12:00:00Z");

  function entry(overrides: Partial<DiagnosticEntry> = {}): DiagnosticEntry {
    return {
      id: 1,
      at: Date.parse("2026-08-25T11:00:00Z"),
      severity: "error",
      source: "coverage",
      message: "boom",
      count: 1,
      ...overrides,
    };
  }

  it("stamps new entries with the running build", () => {
    const store = createDiagnostics({ storage: null, now: () => 0 });
    store.error("coverage", "boom");
    expect(get(store)[0].version).toBe(APP_VERSION);
  });

  it("injects a real version rather than falling back to unknown", () => {
    // If the build-time define is ever dropped, this catches it before the
    // stamp silently degrades to a constant that says nothing.
    expect(APP_VERSION).not.toBe("unknown");
    expect(APP_VERSION).toMatch(/^\d+\.\d+\.\d+$/);
  });

  it("says nothing when the entry came from the running build", () => {
    const report = formatDiagnosticReport([entry({ version: "0.0.3" })], generatedAt, "0.0.3");
    expect(report).toContain("] ERROR (coverage)\n");
    expect(report).not.toContain("recorded by");
  });

  it("names the recording build when it differs from the running one", () => {
    const report = formatDiagnosticReport([entry({ version: "0.0.2" })], generatedAt, "0.0.3");
    expect(report).toContain("[recorded by 0.0.2, now running 0.0.3]");
  });

  it("marks an entry written before stamping existed", () => {
    const report = formatDiagnosticReport([entry()], generatedAt, "0.0.3");
    expect(report).toContain("[recorded by an earlier build, now running 0.0.3]");
  });

  it("names the running build in the report header", () => {
    const report = formatDiagnosticReport([entry({ version: "0.0.3" })], generatedAt, "0.0.3");
    expect(report).toContain("Generated: 2026-08-25T12:00:00.000Z by GitPulse 0.0.3");
  });

  it("never folds a repeat into an entry from a different build", () => {
    // Coalescing rewrites the timestamp, so merging across builds would claim
    // the older build produced something it never saw.
    const storage = memoryStorage({
      [DIAGNOSTIC_STORAGE_KEY]: JSON.stringify([
        { id: 1, at: 1, severity: "error", source: "coverage", message: "boom", count: 1, version: "0.0.1" },
      ]),
    });
    const store = createDiagnostics({ storage, now: () => 99 });
    store.error("coverage", "boom");
    const entries = get(store);
    expect(entries).toHaveLength(2);
    expect(entries[0].version).toBe(APP_VERSION);
    expect(entries[0].count).toBe(1);
    expect(entries[1].version).toBe("0.0.1");
    expect(entries[1].at).toBe(1);
  });

  it("still coalesces repeats from the same build", () => {
    const store = createDiagnostics({ storage: null, now: () => 7 });
    store.error("coverage", "boom");
    store.error("coverage", "boom");
    const entries = get(store);
    expect(entries).toHaveLength(1);
    expect(entries[0].count).toBe(2);
  });

  it("refuses a malformed version from a hostile persisted blob", () => {
    const storage = memoryStorage({
      [DIAGNOSTIC_STORAGE_KEY]: JSON.stringify([
        { id: 4, at: 4, severity: "error", source: "s", message: "object", count: 1, version: { evil: true } },
        { id: 3, at: 3, severity: "error", source: "s", message: "blank", count: 1, version: "   " },
        { id: 2, at: 2, severity: "error", source: "s", message: "oversized", count: 1, version: "9".repeat(500) },
        { id: 1, at: 1, severity: "error", source: "s", message: "fine", count: 1, version: "0.0.2" },
      ]),
    });
    const entries = get(createDiagnostics({ storage }));
    const byMessage = Object.fromEntries(entries.map((e) => [e.message, e.version]));
    expect(byMessage.object).toBeUndefined();
    expect(byMessage.blank).toBeUndefined();
    expect(byMessage.oversized).toHaveLength(32);
    expect(byMessage.fine).toBe("0.0.2");
  });
});

describe("diagnosticFingerprint", () => {
  const same = (a: string, b: string) =>
    expect(diagnosticFingerprint(a)).toBe(diagnosticFingerprint(b));
  const differ = (a: string, b: string) =>
    expect(diagnosticFingerprint(a)).not.toBe(diagnosticFingerprint(b));

  it("masks ISO-8601 timestamps", () => {
    same("failed at 2026-08-30T07:23:44.575Z", "failed at 2026-01-02T00:00:00.000Z");
  });

  it("masks wall-clock times", () => {
    same("started 10:22:44", "started 23:01:02");
  });

  it("masks elapsed durations whatever the unit", () => {
    same("no tests ran in 17.30s", "no tests ran in 17.00s");
    same("no tests ran in 17.30s", "no tests ran in 2ms");
  });

  it("masks heap addresses and handles", () => {
    same("segfault at 0xdeadbeef", "segfault at 0x1");
  });

  it("masks UUIDs", () => {
    same(
      "job 3f2504e0-4f89-11d3-9a0c-0305e82c3301 died",
      "job 00000000-0000-0000-0000-000000000000 died",
    );
  });

  it("masks per-run temporary directories", () => {
    same("wrote /var/folders/ab/T/tmp123/out.xml", "wrote /tmp/other-99/out.xml");
  });

  it("masks the character count in its own elision notice", () => {
    same("head\n… 17064 characters elided …\ntail", "head\n… 3 characters elided …\ntail");
  });

  it("keeps exit codes distinct", () => {
    differ('command "x" failed (exit 3)', 'command "x" failed (exit 1)');
  });

  it("keeps source locations distinct", () => {
    differ("at stress_test.py:944", "at stress_test.py:945");
  });

  it("keeps repository paths distinct", () => {
    differ("scanning /Users/me/Code/Manvi", "scanning /Users/me/Code/GitPulse");
  });

  it("leaves a message with nothing volatile exactly as it is", () => {
    expect(diagnosticFingerprint("clone failed")).toBe("clone failed");
  });

  it("never throws, whatever the input", () => {
    const hostile = ["", " ".repeat(100), "…".repeat(2000), "0x".repeat(1000), "9".repeat(4000)];
    for (const input of hostile) {
      expect(() => diagnosticFingerprint(input)).not.toThrow();
      expect(typeof diagnosticFingerprint(input)).toBe("string");
    }
  });
});

describe("coalescing across per-run detail (regression)", () => {
  const generatedAt = new Date("2026-08-25T12:00:00Z");
  const run = (seconds: string) =>
    [
      'Coverage command ".venv/bin/python -m pytest --cov" failed (exit 3):',
      "INTERNALERROR> SystemExit: 0",
      `=========== no tests ran in ${seconds} ===========`,
    ].join("\n");

  it("folds repeats whose only difference is an embedded duration", () => {
    // Exact-equality coalescing was defeated by pytest stamping its own
    // runtime into the output: three runs of one unchanged failure became
    // three "distinct" entries. Anything a tool embeds per run — a duration,
    // a timestamp, a temp path — defeats it the same way, so a real error
    // storm could flush all 500 slots instead of collapsing into a counter.
    const { store } = makeStore([1, 2]);
    store.error("coverage", run("17.30s"));
    store.error("coverage", run("17.00s"));
    const entries = get(store);
    expect(entries).toHaveLength(1);
    expect(entries[0].count).toBe(2);
  });

  it("retains the most recent occurrence's text, not the first", () => {
    // `at` moves to the newest occurrence, so the message must move with it;
    // keeping the first would date one occurrence and quote another.
    const { store } = makeStore([1, 2]);
    store.error("coverage", run("17.30s"));
    store.error("coverage", run("17.00s"));
    const [head] = get(store);
    expect(head.message).toContain("17.00s");
    expect(head.message).not.toContain("17.30s");
    expect(head.at).toBe(2);
  });

  it("marks a folded group whose occurrences were not identical", () => {
    const { store } = makeStore([1, 2]);
    store.error("coverage", run("17.30s"));
    store.error("coverage", run("17.00s"));
    expect(get(store)[0].varied).toBe(true);
  });

  it("leaves byte-identical repeats unmarked", () => {
    const { store } = makeStore([1, 2]);
    store.error("coverage", run("17.30s"));
    store.error("coverage", run("17.30s"));
    const [head] = get(store);
    expect(head.count).toBe(2);
    expect(head.varied ?? false).toBe(false);
  });

  it("discloses in the report that a folded group is not one verbatim message", () => {
    // `x2` alone would claim two identical occurrences. A summary must never
    // read as more exact than the evidence behind it.
    const varied = formatDiagnosticReport(
      [{ id: 1, at: 0, severity: "error", source: "coverage", message: "boom", count: 2, varied: true, version: "0.0.3" }],
      generatedAt,
      "0.0.3",
    );
    expect(varied).toContain("occurrences differed; showing the most recent");
    const identical = formatDiagnosticReport(
      [{ id: 1, at: 0, severity: "error", source: "coverage", message: "boom", count: 2, version: "0.0.3" }],
      generatedAt,
      "0.0.3",
    );
    expect(identical).not.toContain("occurrences differed");
  });

  it("survives a storm of messages that differ only by timestamp", () => {
    const storage = memoryStorage({
      [DIAGNOSTIC_STORAGE_KEY]: JSON.stringify([
        { id: 1, at: 1, severity: "error", source: "repo", message: "the original failure", count: 1, version: APP_VERSION },
      ]),
    });
    const store = createDiagnostics({ storage, now: () => 5 });
    const storm = MAX_DIAGNOSTIC_ENTRIES + 100;
    for (let i = 0; i < storm; i += 1) {
      const stamp = `2026-08-30T07:${String(i % 60).padStart(2, "0")}:00.000Z`;
      store.error("console", `[${stamp}] socket closed`);
    }
    const entries = get(store);
    expect(entries).toHaveLength(2);
    expect(entries[0].count).toBe(storm);
    expect(entries[1].message).toBe("the original failure");
  });

  it("does not fold failures that differ in exit code", () => {
    const { store } = makeStore([1, 2]);
    store.error("coverage", 'command "x" failed (exit 3): boom');
    store.error("coverage", 'command "x" failed (exit 1): boom');
    expect(get(store)).toHaveLength(2);
  });

  it("does not fold failures that differ in source location", () => {
    const { store } = makeStore([1, 2]);
    store.error("coverage", "aborted at stress_test.py:944");
    store.error("coverage", "aborted at stress_test.py:945");
    expect(get(store)).toHaveLength(2);
  });

  it("still refuses to fold across builds when only per-run detail differs", () => {
    const storage = memoryStorage({
      [DIAGNOSTIC_STORAGE_KEY]: JSON.stringify([
        { id: 1, at: 1, severity: "error", source: "coverage", message: run("17.30s"), count: 1, version: "0.0.1" },
      ]),
    });
    const store = createDiagnostics({ storage, now: () => 9 });
    store.error("coverage", run("17.00s"));
    const entries = get(store);
    expect(entries).toHaveLength(2);
    expect(entries[1].at).toBe(1);
  });

  it("refuses a malformed varied flag from a hostile persisted blob", () => {
    const storage = memoryStorage({
      [DIAGNOSTIC_STORAGE_KEY]: JSON.stringify([
        { id: 2, at: 2, severity: "error", source: "s", message: "truthy", count: 2, varied: "yes" },
        { id: 1, at: 1, severity: "error", source: "s", message: "real", count: 2, varied: true },
      ]),
    });
    const entries = get(createDiagnostics({ storage }));
    expect(entries.find((e) => e.message === "truthy")?.varied).toBeUndefined();
    expect(entries.find((e) => e.message === "real")?.varied).toBe(true);
  });
});

describe("diagnostics ring under fuzz", () => {
  /** Deterministic LCG: the same sequence every run, so a failure reproduces. */
  function seeded(seed: number) {
    let state = seed >>> 0;
    return () => {
      state = (state * 1664525 + 1013904223) >>> 0;
      return state / 0x100000000;
    };
  }

  const shapes = [
    (n: number) => `Coverage command failed (exit ${n % 4}): no tests ran in ${n % 90}.${n % 100}s`,
    (n: number) => `[2026-08-30T07:${String(n % 60).padStart(2, "0")}:0${n % 10}.000Z] socket closed`,
    (n: number) => `wrote /var/folders/ab/T/tmp${n}/coverage.xml`,
    (n: number) => `panic at 0x${(n * 2654435761).toString(16)}`,
    (n: number) => `clone failed for repo-${n % 7}`,
    () => "plain unchanging failure",
  ];

  it("holds its invariants across a random operation stream", () => {
    const random = seeded(20260830);
    const storage = memoryStorage();
    const store = createDiagnostics({ storage, now: () => 1 });
    let recorded = 0;

    for (let i = 0; i < 4000; i += 1) {
      const shape = shapes[Math.floor(random() * shapes.length)];
      const message = shape(Math.floor(random() * 1000));
      if (random() < 0.5) store.error("fuzz", message);
      else store.warn("fuzz", message);
      recorded += 1;
    }

    const entries = get(store);
    expect(entries.length).toBeLessThanOrEqual(MAX_DIAGNOSTIC_ENTRIES);
    for (let i = 1; i < entries.length; i += 1) {
      expect(entries[i - 1].id).toBeGreaterThan(entries[i].id);
    }
    for (const entry of entries) {
      expect(entry.count).toBeGreaterThanOrEqual(1);
      expect(entry.message.length).toBeLessThanOrEqual(2000);
      // A single occurrence cannot have diverged from anything.
      if (entry.varied) expect(entry.count).toBeGreaterThan(1);
    }
    // At this size the ring overflows, so the surviving counts can only be a
    // subset — never more than what actually happened.
    const counted = entries.reduce((total, entry) => total + entry.count, 0);
    expect(entries).toHaveLength(MAX_DIAGNOSTIC_ENTRIES);
    expect(counted).toBeLessThanOrEqual(recorded);

    // The persisted blob restores an identical ring.
    expect(get(createDiagnostics({ storage }))).toEqual(entries);
  }, STRESS_TIMEOUT_MS);

  it("accounts for every event exactly while the ring has room", () => {
    const random = seeded(7);
    const store = createDiagnostics({ storage: memoryStorage(), now: () => 1 });
    let recorded = 0;
    for (let i = 0; i < 200; i += 1) {
      const shape = shapes[Math.floor(random() * shapes.length)];
      store.error("fuzz", shape(Math.floor(random() * 5)));
      recorded += 1;
    }
    const entries = get(store);
    expect(entries.length).toBeLessThan(MAX_DIAGNOSTIC_ENTRIES);
    expect(entries.reduce((total, entry) => total + entry.count, 0)).toBe(recorded);
  });

  it("normalizes a worst-case message well inside its per-event budget", () => {
    // The fingerprint runs on every recorded error, so a storm must not be
    // able to stall the UI thread on regex work. Input is bounded by the
    // 2000-char clamp, and the patterns have no nested quantifiers, so there
    // is nothing here that can backtrack catastrophically.
    const worst = Array.from({ length: 40 }, (_, i) =>
      `2026-08-30T07:23:${String(i % 60).padStart(2, "0")}.575Z ran in ${i}.${i}s at 0x${i.toString(16)} in /var/folders/ab/T/tmp${i}/x`,
    ).join("\n");
    const started = performance.now();
    for (let i = 0; i < 1000; i += 1) diagnosticFingerprint(worst);
    const perCall = (performance.now() - started) / 1000;
    expect(perCall).toBeLessThan(1);
  });
});

describe("credentials named by a JSON object key", () => {
  const SECRET = "SUPERSECRETVALUE1234567890";

  // Before this, the object traversal handed each value to the contextual
  // stage with its key discarded, so an opaque token matched nothing and the
  // whole document was reported and persisted unchanged. Every entry below
  // leaked in full.
  it.each([
    ["client_secret", `{"client_secret":"${SECRET}"}`],
    ["access_token", `{"access_token":"${SECRET}"}`],
    ["camelCase clientSecret", `{"clientSecret":"${SECRET}"}`],
    ["header-style x-api-key", `{"x-api-key":"${SECRET}"}`],
    ["Authorization", `{"Authorization":"Bearer ${SECRET}"}`],
    ["nested under an ordinary key", `{"cfg":{"password":"${SECRET}"}}`],
    ["vendor-prefixed github_token", `{"github_token":"${SECRET}"}`],
    ["Cookie", `{"Cookie":"session=${SECRET}"}`],
  ])("redacts %s", (_label, document) => {
    const out = redactDiagnosticText(document);
    expect(out, `leaked: ${out}`).not.toContain(SECRET);
  });

  it("fails closed on a non-string value under a credential key", () => {
    // A number, array or object under a credential key is still the
    // credential; recursing into it would leave it whole.
    for (const document of [
      '{"password":1234567890}',
      '{"token":["a","b"]}',
      '{"secret":{"inner":"value"}}',
    ]) {
      expect(redactDiagnosticText(document), document).toContain("<redacted>");
    }
  });

  it("is idempotent, so a clean value never reads as secret-bearing", () => {
    const once = redactDiagnosticText(`{"access_token":"${SECRET}"}`);
    expect(redactDiagnosticText(once)).toBe(once);
  });

  it("leaves benign key-shaped names alone", () => {
    // The suffix rule deliberately excludes the bare word `key`: redacting
    // these would strip the report of the facts it exists to carry.
    const out = redactDiagnosticText(
      '{"public_key":"ssh-ed25519 AAAA","cache_key":"abc","primary_key":"id"}',
    );
    expect(out).toContain("abc");
    expect(out).toContain("id");
  });

  it("normalizes field names across naming styles", () => {
    expect(normalizeFieldName("clientSecret")).toBe("client_secret");
    expect(normalizeFieldName("X-Api-Key")).toBe("x_api_key");
    expect(normalizeFieldName("ACCESS_TOKEN")).toBe("access_token");
    expect(normalizeFieldName("accessToken")).toBe("access_token");
    expect(isSecretFieldName("APIKey")).toBe(true);
    expect(isSecretFieldName("public_key")).toBe(false);
  });
});

describe("credentials that appear as a JSON object key", () => {
  const GHP = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";

  it.each([
    ["at the top level", `{"${GHP}":"x"}`],
    ["nested in a cache map", `{"cache":{"${GHP}":1}}`],
  ])("redacts a vendor token used as a key %s", (_label, document) => {
    const out = redactDiagnosticText(document);
    expect(out, `leaked: ${out}`).not.toContain(GHP);
    expect(() => JSON.parse(out), `invalid JSON: ${out}`).not.toThrow();
  });

  it("keeps both entries when two keys redact to the same text", () => {
    // Two distinct tokens of equal length redact to identical text.
    // Overwriting would make the document claim there was only ever one.
    const other = "ghp_ZYXWVUTSRQPONMLKJIHGFEDCBA9876543210B";
    const out = redactDiagnosticText(`{"${GHP}":1,"${other}":2}`);
    expect(Object.keys(JSON.parse(out)), `an entry was dropped: ${out}`).toHaveLength(2);
  });

  it("is idempotent", () => {
    const once = redactDiagnosticText(`{"${GHP}":"x"}`);
    expect(redactDiagnosticText(once)).toBe(once);
  });
});

describe("redaction cost stays linear in the number of object keys", () => {
  it("does not blow up when every key redacts to the same text", () => {
    // The adversarial shape is also the cheapest to mount: N distinct tokens of
    // equal length all redact to identical text, so a rename that probes for a
    // free name linearly from 2 pays n probes on key n. Measured at 20k such
    // keys: 31s while probing, 277ms with a per-base counter. Quadratic misses
    // this budget by 3x; linear clears it by 36x.
    const hostile: Record<string, unknown> = {};
    for (let index = 0; index < 20000; index += 1) {
      hostile[`ghp_${String(index).padStart(36, "0")}`] = index;
    }
    const started = Date.now();
    const out = redactDiagnosticText(JSON.stringify(hostile));
    const elapsed = Date.now() - started;

    expect(out).not.toContain("ghp_000000000000000000000000000000000001");
    // Collision disambiguation must not drop entries: 20k keys in, 20k out.
    expect(Object.keys(JSON.parse(out)), "entries were dropped").toHaveLength(20000);
    expectWithinBudget(elapsed, 2000, "redaction over 20k colliding keys");
  }, STRESS_TIMEOUT_MS);

  it("terminates on a deeply nested document", () => {
    let document = '"leaf"';
    for (let level = 0; level < 200; level += 1) document = `{"level_${level}":${document}}`;
    expect(redactDiagnosticText(document).length).toBeGreaterThan(0);
  });
});

describe("the key rebuild preserves exotic keys", () => {
  const GHP = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";

  it.each([["__proto__"], ["constructor"], ["prototype"], ["toString"]])(
    "keeps a %s entry when a sibling key is renamed",
    (exotic) => {
      // JSON.parse creates `__proto__` as a genuine own property, so a document
      // can carry one. Writing it back with `value[key] = entry` reaches
      // Object.prototype's setter instead of storing anything, which silently
      // shrank the report by one entry.
      const out = redactDiagnosticText(`{"${GHP}":1,"${exotic}":2}`);
      expect(out, `leaked: ${out}`).not.toContain(GHP);
      expect(Object.keys(JSON.parse(out)), `entry lost: ${out}`).toHaveLength(2);
    },
  );
});
