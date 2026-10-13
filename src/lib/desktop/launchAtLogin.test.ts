import { describe, expect, it } from "vitest";
import { readLaunchAtLogin, writeLaunchAtLogin, type Autostart } from "./launchAtLogin";

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
    expect(await writeLaunchAtLogin(true, failEnable)).toEqual({ state: "off" });
    expect(await writeLaunchAtLogin(true, broken)).toMatchObject({ state: "unknown", error: "autostart plugin not available" });
  });
});
