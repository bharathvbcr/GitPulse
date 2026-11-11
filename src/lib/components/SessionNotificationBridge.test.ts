import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

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
