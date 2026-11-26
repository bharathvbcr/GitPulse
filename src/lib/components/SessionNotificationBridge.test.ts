import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";
import { describe, expect, it } from "vitest";
import SessionNotificationBridge from "./SessionNotificationBridge.svelte";

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
  const source = read("src", "lib", "components", "SessionNotificationBridge.svelte");

  it("compiles cleanly and imports Logo inside aria-hidden", () => {
    const { warnings } = compile(source, { generate: "client", filename: "SessionNotificationBridge.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
    expect(source).toMatch(/import Logo from "\.\/Logo\.svelte";/);
    expect(source).toMatch(/aria-hidden="true"[^>]*><Logo\b/);
  });

  it("renders nothing when no notice is active", () => {
    const { body } = render(SessionNotificationBridge);
    expect(body).not.toContain("session-activation");
  });

  it("renders .session-activation with the current Waylode badge Logo when a notice is active", () => {
    const { body } = render(SessionNotificationBridge, {
      props: { initialMissed: "That terminal session has ended." },
    });
    expect(body).toMatch(/class="session-activation gp-card shadow-float\b/);
    expect(body).toContain("That terminal session has ended.");
    expect(body).toContain("M23.8 9.0A10.5 10.5 0 1 0 26.5 16.0H21.0L16.2 11.6L11.4 18.8");
    expect(body).toMatch(/id="wl-[a-z0-9]+-badge"/);
    expect(body).toMatch(/<span class="session-activation-logo[^"]*" aria-hidden="true">[\s\S]*?<svg[^>]*aria-label="Waylode"/);
  });
});
