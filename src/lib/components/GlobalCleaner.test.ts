import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "GlobalCleaner.svelte"),
  "utf8",
);

const refreshBody = () => {
  const start = source.indexOf("async function refresh()");
  return source.slice(start, source.indexOf("function edited()", start));
};

describe("GlobalCleaner status read failures", () => {
  it("keeps the status-read error apart from action errors", () => {
    expect(source).toContain("let refreshError = $state<string | null>(null);");
    const body = refreshBody();
    // A poll writes only refreshError: it must neither set nor clear a
    // save/run/inspect error the reader has not seen yet.
    expect(body).toContain("refreshError = formatError(e)");
    expect(body).toContain("refreshError = null;");
    expect(body).not.toMatch(/\berror = /);
  });

  it("clears the read error only on a successful read", () => {
    const body = refreshBody();
    const ok = body.slice(body.indexOf("accept(next)"), body.indexOf("} catch"));
    expect(ok).toContain("refreshError = null;");
    expect(body.slice(body.indexOf("} catch"))).not.toContain("refreshError = null");
  });

  it("shows both errors in an alert", () => {
    expect(source).toContain('{#if error}<p role="alert"');
    expect(source).toContain('{#if refreshError}<p role="alert"');
  });

  it("offers Try again instead of Loading forever when the first read failed", () => {
    const at = source.indexOf("{:else if refreshError}");
    expect(at).toBeGreaterThan(-1);
    const loading = source.indexOf("Loading saved cleanup policy…");
    expect(loading).toBeGreaterThan(at);
    const failed = source.slice(at, loading);
    expect(failed).toContain("Couldn't read the cleanup policy.");
    expect(failed).toContain("onclick={refresh}");
    expect(failed).toContain("Try again");
  });

  it("formats every rejection with formatError, never String(e)", () => {
    expect(source).toContain('import { formatError } from "../ui/formatError";');
    expect(source).not.toContain("String(e)");
    expect(source.match(/formatError\(e\)/g)?.length).toBe(7);
  });
});
