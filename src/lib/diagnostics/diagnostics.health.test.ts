import { get } from "svelte/store";
import { describe, expect, it } from "vitest";
import { createDiagnostics, DIAGNOSTIC_STORAGE_KEY, formatDiagnosticReport, redactDiagnosticText, SECRET_FIELD_NAMES } from "./diagnostics";
import { memoryStorage } from "../repos/persist";

describe("diagnostics retention and durability", () => {
  it("rejects saved dates outside JavaScript's calendar range without crashing the report", () => {
    const storage = memoryStorage({ [DIAGNOSTIC_STORAGE_KEY]: JSON.stringify([
      { id: 1, at: 1e100, severity: "error", source: "test", message: "invalid clock" },
      { id: 2, at: 0, severity: "error", source: "test", message: "valid entry" },
    ]) });
    const store = createDiagnostics({ storage });
    expect(() => formatDiagnosticReport(get(store))).not.toThrow();
    expect(get(store.health).restorationError).toContain("1 invalid");
    expect(get(store)[0].message).toBe("valid entry");
  });
  it("keeps identical errors from two builds distinct after restart", () => {
    const storage = memoryStorage();
    createDiagnostics({ storage, buildId: "build-one" }).error("runtime", "same failure");
    const next = createDiagnostics({ storage, buildId: "build-two" });
    next.error("runtime", "same failure");
    const entries = get(next);
    expect(entries).toHaveLength(2);
    expect(entries.map(entry => entry.buildId)).toEqual(["build-two", "build-one"]);
    const report = formatDiagnosticReport(entries);
    expect(report).toContain("build-one");
    expect(report).toContain("build-two");
  });
  it("uses the credential-name table for plain diagnostic assignments too", () => {
    for (const name of SECRET_FIELD_NAMES) {
      expect(redactDiagnosticText(`failed ${name}=private-value`), name).not.toContain("private-value");
    }
    expect(redactDiagnosticText("public_key=visible cache_key=visible")).toBe("public_key=visible cache_key=visible");
  });
  it("includes unavailable persistence in a copied report even with no entries", () => {
    const store = createDiagnostics({ storage: null });
    const report = formatDiagnosticReport([], new Date(0), "test", get(store.health));
    expect(report).toContain("memory only");
    expect(report).toContain("Persistent storage is unavailable");
  });
  it("retains module, WebView, and observer failures in production", () => {
    const store = createDiagnostics({ storage: memoryStorage(), development: false });
    for (const text of ["Importing a module script failed.", "undefined is not an object (evaluating 'module.default')", "ResizeObserver loop limit exceeded", "[TAURI] Couldn't find callback id 123"]) store.error("runtime", text);
    expect(get(store)).toHaveLength(4);
    expect(get(store.health).suppressedRuntimeEvents).toBe(0);
  });

  it("only suppresses identifiable development reload chatter and counts it", () => {
    const store = createDiagnostics({ storage: null, development: true });
    for (let i = 0; i < 1_000; i++) store.warn("console", "[hmr] Failed to reload /src/App.svelte");
    store.error("runtime", "Importing a module script failed.");
    expect(get(store)).toHaveLength(1);
    expect(get(store.health).suppressedRuntimeEvents).toBe(1_000);
  });

  it("exposes failed writes, bounds retries during a storm, and recovers the full ring", () => {
    const backing = memoryStorage();
    let failing = true;
    let attempts = 0;
    const store = createDiagnostics({ storage: {
      getItem: backing.getItem,
      removeItem: backing.removeItem,
      setItem: (key, value) => { attempts++; if (failing) throw new Error("quota token=private-value"); backing.setItem(key, value); },
    } });
    for (let i = 0; i < 100; i++) store.error("test", `failure ${i}`);
    expect(get(store)).toHaveLength(100);
    expect(get(store.health).persistence).toBe("memory-only");
    expect(get(store.health).persistenceError).not.toContain("private-value");
    expect(attempts).toBe(1);
    failing = false;
    store.retryPersistence();
    expect(get(store.health).persistence).toBe("saved");
    expect(get(createDiagnostics({ storage: backing }))).toHaveLength(100);
  });

  it("distinguishes missing storage, an unreadable history, and corrupt JSON from an empty log", () => {
    expect(get(createDiagnostics({ storage: null }).health).persistence).toBe("memory-only");
    const unreadable = createDiagnostics({ storage: { getItem: () => { throw new Error("read denied"); }, setItem: () => {}, removeItem: () => {} } });
    expect(get(unreadable.health).restorationError).toContain("read denied");
    const corrupt = createDiagnostics({ storage: memoryStorage({ [DIAGNOSTIC_STORAGE_KEY]: "{broken" }) });
    expect(get(corrupt.health).restorationError).toBeTruthy();
    expect(get(createDiagnostics({ storage: memoryStorage() }).health).restorationError).toBeNull();
  });

  it("does not claim a failed persistent clear succeeded", () => {
    const backing = memoryStorage();
    const store = createDiagnostics({ storage: { ...backing, removeItem: () => { throw new Error("read-only"); } } });
    store.error("test", "old entry");
    store.clear();
    expect(get(store)).toEqual([]);
    expect(get(store.health).persistence).toBe("memory-only");
    store.retryPersistence();
    expect(get(store.health).persistence).toBe("saved");
    expect(get(createDiagnostics({ storage: backing }))).toEqual([]);
  });

  it("repairs duplicate and exhausted persisted identities without dropping valid events", () => {
    const storage = memoryStorage({ [DIAGNOSTIC_STORAGE_KEY]: JSON.stringify([
      { id: Number.MAX_SAFE_INTEGER, at: 2, severity: "error", source: "test", message: "first" },
      { id: Number.MAX_SAFE_INTEGER, at: 1, severity: "error", source: "test", message: "second" },
    ]) });
    const store = createDiagnostics({ storage });
    store.error("test", "third"); store.error("test", "fourth");
    const entries = get(store);
    expect(entries).toHaveLength(4);
    expect(new Set(entries.map(e => e.id)).size).toBe(4);
    expect(entries.every(e => Number.isSafeInteger(e.id))).toBe(true);
  });
});
