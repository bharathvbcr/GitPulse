import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { compile } from "svelte/compiler";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const read = (...path: string[]) => readFileSync(join(root, ...path), "utf8");

/**
 * SessionNotificationBridge stays in the entry chunk, on purpose.
 *
 * Moving it behind LazyMount would save ~4.3 kB, and it would lose things:
 *
 *  - A clicked session banner has no durable claim. For an activity banner the
 *    backend writes the activation to the store, so the lazy NativeNotification
 *    bridge reads it on mount (harness/onboarding.html proves that). A session
 *    banner skips the store and only emits `gitpulse-session-notification-open`
 *    (`Some(key) => opening.emit(...)` in src-tauri/src/workbench/notifications.rs),
 *    so an event that arrives before a lazily mounted listener exists is gone and
 *    the click does nothing.
 *  - It adopts the terminal sessions the host kept running across a reload
 *    (`adoptDetachedSessions`) and binds the attention stream once per page.
 *    Both are start-up work that the terminal dock and the Sessions list read.
 *
 * This pins the placement so a later bundle squeeze does not move it without
 * replacing the event with something durable first.
 */
describe("SessionNotificationBridge placement", () => {
  const app = read("src", "App.svelte");

  it("is mounted directly by the app, not through LazyMount", () => {
    expect(app).toMatch(/import SessionNotificationBridge from "\.\/lib\/components\/SessionNotificationBridge\.svelte";/);
    expect(app).toContain("<SessionNotificationBridge />");
    expect(app).not.toMatch(/load[A-Za-z]*SessionNotificationBridge/);
  });

  it("is not on the list of components kept out of the entry chunk", () => {
    expect(read("src", "lib", "components", "lazyLoaders.ts")).not.toContain("SessionNotificationBridge");
  });

  it("depends on an event the backend does not persist for session banners", () => {
    const backend = read("src-tauri", "src", "workbench", "notifications.rs");
    expect(backend).toContain('Some(key) => opening.emit("gitpulse-session-notification-open", key.clone())');
    expect(backend).toMatch(/if session\.is_none\(\) \{\s*if let Err\(error\) = activate\(&host, &native\)/);
  });
});

describe("SessionNotificationBridge branding and compilation", () => {
  const here = dirname(fileURLToPath(import.meta.url));
  const source = read("src", "lib", "components", "SessionNotificationBridge.svelte");
  const scratch = join(root, "tmp");
  mkdirSync(scratch, { recursive: true });
  const generatedDir = mkdtempSync(join(scratch, "vitest-session-bridge-"));
  const generatedPath = join(generatedDir, "SessionNotificationBridgeMissed.svelte");
  const MISSED_LINE = "let missed = $state<string | null>(null);";

  beforeAll(() => {
    expect(source).toContain(MISSED_LINE);
    const renderedCopy = source
      .replace(MISSED_LINE, 'let missed = $state<string | null>("That terminal session has ended.");')
      .replace(/from "(\.\.?\/[^"]+)"/g, (_m, spec: string) => `from "${join(here, spec).replaceAll("\\", "/")}"`)
      .replace(/from "\.\/Logo\.svelte"/g, `from "${join(here, "Logo.svelte").replaceAll("\\", "/")}"`);
    writeFileSync(generatedPath, renderedCopy);
  });

  afterAll(() => {
    rmSync(generatedDir, { recursive: true, force: true });
  });

  it("compiles cleanly and imports Logo inside aria-hidden", () => {
    const { warnings } = compile(source, { generate: "client", filename: "SessionNotificationBridge.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
    expect(source).toMatch(/import Logo from "\.\/Logo\.svelte";/);
    expect(source).toMatch(/aria-hidden="true"[^>]*><Logo\b/);
  });

  it("renders .session-activation with the current GitPulse badge Logo when a notice is active", async () => {
    const component = (await import(/* @vite-ignore */ generatedPath)).default;
    const { render } = await import("svelte/server");
    const { body } = render(component);
    expect(body).toMatch(/class="session-activation gp-card shadow-float\b/);
    expect(body).toContain("That terminal session has ended.");
    expect(body).toContain("M24.04 9.25A10.5 10.5 0 1 0 26.5 16H22.6L20.2 21L16.8 9.9L14.4 16H11.8");
    expect(body).toMatch(/id="gp-[a-z0-9]+-badge"/);
  });
});
