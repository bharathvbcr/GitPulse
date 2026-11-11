import { describe, expect, it } from "vitest";
import { describeLaunchAtLogin, readLaunchAtLogin, writeLaunchAtLogin, type Autostart } from "./launchAtLogin";

const plugin = (over: Partial<Autostart> = {}): (() => Promise<Autostart>) => async () => ({
  isEnabled: async () => true, enable: async () => {}, disable: async () => {}, ...over,
});
const broken = async (): Promise<Autostart> => { throw new Error("autostart plugin not available"); };

describe("ft-038e6ba5 Launch at login", () => {
  it("reports on and off as read", async () => {
    expect(await readLaunchAtLogin(plugin({ isEnabled: async () => true }))).toEqual({ state: "on" });
    expect(await readLaunchAtLogin(plugin({ isEnabled: async () => false }))).toEqual({ state: "off" });
  });

  it("a plugin that throws is unknown with the reason, never Off", async () => {
    expect(await readLaunchAtLogin(broken)).toEqual({ state: "unknown", error: "autostart plugin not available" });
    expect(await readLaunchAtLogin(plugin({ isEnabled: async () => { throw new Error("denied"); } }))).toMatchObject({ state: "unknown", error: "denied" });
  });

  it("reports the state read back after a change", async () => {
    let on = false;
    const load = plugin({ enable: async () => { on = true; }, isEnabled: async () => on });
    expect(await writeLaunchAtLogin(true, load)).toEqual({ state: "on" });
  });

  it("a failed change reports the real state, or unknown when that cannot be read", async () => {
    const failEnable = plugin({ enable: async () => { throw new Error("denied"); }, isEnabled: async () => false });
    // Intentional contract change (SO-5): the read-back state now carries the
    // failure, so the row can say the OS refused instead of the switch silently
    // snapping back. This was `{ state: "off" }` and dropped "denied".
    expect(await writeLaunchAtLogin(true, failEnable)).toEqual({ state: "off", error: "denied" });
    expect(await writeLaunchAtLogin(true, broken)).toMatchObject({ state: "unknown", error: "autostart plugin not available" });
  });

  it("a failed disable reads back on and carries the failure", async () => {
    const failDisable = plugin({ disable: async () => { throw new Error("locked"); }, isEnabled: async () => true });
    expect(await writeLaunchAtLogin(false, failDisable)).toEqual({ state: "on", error: "locked" });
  });

  it("a successful change carries no error key", async () => {
    const done = await writeLaunchAtLogin(true, plugin({ isEnabled: async () => true }));
    expect(done).toEqual({ state: "on" });
    expect("error" in done).toBe(false);
  });
});

describe("describeLaunchAtLogin", () => {
  it("shows Checking... with the toggle disabled while the first read is out, not 'could not be read'", () => {
    const row = describeLaunchAtLogin({ state: "loading" });
    expect(row.disabled).toBe(true);
    expect(row.note).toEqual({ tone: "muted", text: "Checking…" });
  });

  it("an unreadable plugin stays a disabled warning that names the reason", () => {
    const row = describeLaunchAtLogin({ state: "unknown", error: "denied" });
    expect(row.disabled).toBe(true);
    expect(row.note?.tone).toBe("warn");
    expect(row.note?.text).toContain("couldn't be read (denied)");
  });

  it("a refused change leaves the toggle usable and says what the OS reports now", () => {
    const row = describeLaunchAtLogin({ state: "off", error: "denied" });
    expect(row.disabled).toBe(false);
    expect(row.note).toEqual({ tone: "warn", text: "Couldn't change open at login: denied. It is currently off." });
  });

  it("a clean on or off has no note", () => {
    expect(describeLaunchAtLogin({ state: "on" })).toEqual({ disabled: false, note: null });
  });
});
