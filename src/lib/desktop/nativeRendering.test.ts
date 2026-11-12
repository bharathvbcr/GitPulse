import { describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import {
  NATIVE_RENDERING_SOURCE,
  applyNativeRendering,
  failureStatus,
  installNativeRendering,
  nativeRenderingBoot,
  setNativeRenderingBoot,
  parseNativeRendering,
  refusals,
  requestFor,
  type NativeRenderingDeps,
  type NativeRenderingStatus,
} from "./nativeRendering";
import { interfaceStore } from "../stores/interfaceStore";
import { resetHostPlatformForTests } from "../stores/platformStore";

const OFF = { requested: false, active: false, reason: null };
const on = { requested: true, active: true, reason: null };
const refused = (reason: string) => ({ requested: true, active: false, reason });

const MAC = {
  os: "macos",
  dock_hiding: true,
  browser_sessions: true,
  native_material: true,
  liquid_glass: true,
  high_refresh: true,
} as const;

function deps(apply: NativeRenderingDeps["apply"]) {
  const warn = vi.fn<(source: string, message: string) => void>();
  const publish = vi.fn<(status: NativeRenderingStatus) => void>();
  return { apply, warn, publish } satisfies NativeRenderingDeps;
}

describe("parseNativeRendering", () => {
  it("reads the backend's wire shape", () => {
    expect(parseNativeRendering({ liquid_glass: on, high_refresh: refused("why") })).toEqual({
      liquid_glass: on,
      high_refresh: refused("why"),
    });
  });

  it.each([
    ["nothing", undefined],
    ["a string", "ok"],
    ["a missing flag", { liquid_glass: on }],
    ["a non-boolean active", { liquid_glass: { ...on, active: "yes" }, high_refresh: OFF }],
    ["a numeric reason", { liquid_glass: { ...on, reason: 3 }, high_refresh: OFF }],
  ])("reads %s as no answer rather than an applied one", (_label, value) => {
    expect(parseNativeRendering(value)).toBeNull();
  });

  it("never lets a flag be active while it carries a reason", () => {
    const parsed = parseNativeRendering({
      liquid_glass: { requested: true, active: true, reason: "older macOS" },
      high_refresh: OFF,
    });
    expect(parsed?.liquid_glass.active).toBe(false);
    expect(parsed?.liquid_glass.reason).toBe("older macOS");
  });
});

describe("requestFor", () => {
  const prefs = { nativeGlass: true, highRefresh: true };

  it("sends the stored preference where the build has the code", () => {
    expect(requestFor(prefs, { liquid_glass: true, high_refresh: true })).toEqual({
      liquidGlass: true,
      highRefresh: true,
    });
  });

  it("never asks a host for a switch it does not have", () => {
    expect(requestFor(prefs, { liquid_glass: false, high_refresh: false })).toEqual({
      liquidGlass: false,
      highRefresh: false,
    });
  });
});

describe("applyNativeRendering", () => {
  const request = { liquidGlass: true, highRefresh: true };

  it("publishes what the backend observed and records only the refusals", async () => {
    const d = deps(async () => ({ liquid_glass: on, high_refresh: refused("Frame pacing is unchanged: no such feature.") }));
    const status = await applyNativeRendering(request, d);
    expect(status.liquid_glass.active).toBe(true);
    expect(d.publish).toHaveBeenCalledWith(status);
    expect(d.warn).toHaveBeenCalledTimes(1);
    const [source, message] = d.warn.mock.calls[0] as [string, string];
    expect(source).toBe(NATIVE_RENDERING_SOURCE);
    expect(message).toContain("120 Hz rendering");
    expect(message).toContain("no such feature");
  });

  it("records nothing when everything asked for is in effect", async () => {
    const d = deps(async () => ({ liquid_glass: on, high_refresh: on }));
    await applyNativeRendering(request, d);
    expect(d.warn).not.toHaveBeenCalled();
  });

  it("turns a rejected call into a refusal for each switch that was asked for", async () => {
    const d = deps(async () => {
      throw new Error("channel closed");
    });
    const status = await applyNativeRendering({ liquidGlass: true, highRefresh: false }, d);
    expect(status.liquid_glass.active).toBe(false);
    expect(status.liquid_glass.reason).toMatch(/^Couldn't apply the native rendering settings\./);
    expect(status.liquid_glass.reason).toContain("channel closed");
    expect(status.high_refresh).toEqual(OFF);
    expect(d.warn).toHaveBeenCalledTimes(1);
  });

  it("treats an unreadable answer as a failure, not as success", async () => {
    const d = deps(async () => ({ liquid_glass: "on", high_refresh: "on" }));
    const status = await applyNativeRendering(request, d);
    expect(status.liquid_glass.active).toBe(false);
    expect(status.high_refresh.active).toBe(false);
    expect(d.warn).toHaveBeenCalledTimes(2);
  });
});

describe("refusals and failureStatus", () => {
  it("lists a refusal only for a switch that was asked for", () => {
    const status: NativeRenderingStatus = { liquid_glass: OFF, high_refresh: refused("stuck") };
    expect(refusals(status)).toHaveLength(1);
    expect(refusals({ liquid_glass: OFF, high_refresh: OFF })).toEqual([]);
  });

  it("gives a switch nobody asked for no failure to report", () => {
    expect(failureStatus({ liquidGlass: false, highRefresh: false }, "x")).toEqual({
      liquid_glass: OFF,
      high_refresh: OFF,
    });
  });
});

describe("installNativeRendering", () => {
  const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

  function reset(prefs: { nativeGlass: boolean; highRefresh: boolean }) {
    resetHostPlatformForTests({ ...MAC });
    interfaceStore.setNativeGlass(prefs.nativeGlass);
    interfaceStore.setHighRefresh(prefs.highRefresh);
  }

  // Measured in the real app: WebKit keeps the frame-rate feature in the app's
  // user defaults, so a value from an earlier run survived into a run whose
  // preference was off. Boot therefore reconciles even with both switches off.
  it("reconciles once at boot with both switches off, so a stale WebKit value is reset", async () => {
    reset({ nativeGlass: false, highRefresh: false });
    const apply = vi.fn(async () => ({ liquid_glass: OFF, high_refresh: OFF }));
    const stop = installNativeRendering(deps(apply));
    await settle();
    stop();
    expect(apply).toHaveBeenCalledTimes(1);
    expect(apply).toHaveBeenCalledWith({ liquidGlass: false, highRefresh: false });
  });

  it("remembers what the launch first asked for, and never replaces it with a later change", async () => {
    reset({ nativeGlass: false, highRefresh: true });
    setNativeRenderingBoot(null);
    const apply = vi.fn(async () => ({ liquid_glass: OFF, high_refresh: on }));
    const stop = installNativeRendering(deps(apply));
    await settle();
    expect(get(nativeRenderingBoot)).toEqual({ liquidGlass: false, highRefresh: true });
    interfaceStore.setHighRefresh(false);
    await settle();
    stop();
    expect(get(nativeRenderingBoot)).toEqual({ liquidGlass: false, highRefresh: true });
    setNativeRenderingBoot(null);
  });

  it("sends the stored switch on boot and the change when it is made", async () => {
    reset({ nativeGlass: true, highRefresh: false });
    const apply = vi.fn(async () => ({ liquid_glass: on, high_refresh: OFF }));
    const stop = installNativeRendering(deps(apply));
    await settle();
    expect(apply).toHaveBeenLastCalledWith({ liquidGlass: true, highRefresh: false });

    interfaceStore.setNativeGlass(false);
    await settle();
    stop();
    expect(apply).toHaveBeenLastCalledWith({ liquidGlass: false, highRefresh: false });
    expect(apply).toHaveBeenCalledTimes(2);
  });

  it("does not ask a host without the capability", async () => {
    resetHostPlatformForTests({ ...MAC, os: "windows", liquid_glass: false, high_refresh: false, native_material: false });
    interfaceStore.setNativeGlass(true);
    const apply = vi.fn(async () => ({ liquid_glass: OFF, high_refresh: OFF }));
    const stop = installNativeRendering(deps(apply));
    await settle();
    stop();
    interfaceStore.setNativeGlass(false);
    expect(apply).not.toHaveBeenCalled();
  });

  it("keeps only the latest request while one is in flight, in order", async () => {
    reset({ nativeGlass: false, highRefresh: false });
    const releases: Array<() => void> = [];
    const calls: boolean[] = [];
    const apply: NativeRenderingDeps["apply"] = (request) =>
      new Promise((resolve) => {
        calls.push(request.liquidGlass);
        releases.push(() => resolve({ liquid_glass: request.liquidGlass ? on : OFF, high_refresh: OFF }));
      });
    const stop = installNativeRendering(deps(apply));
    await settle();
    expect(calls).toEqual([false]); // the boot reconcile
    releases[0]();
    await settle();

    interfaceStore.setNativeGlass(true);
    await settle();
    interfaceStore.setNativeGlass(false);
    interfaceStore.setNativeGlass(true);
    interfaceStore.setNativeGlass(false);
    await settle();
    expect(calls).toEqual([false, true]);

    releases[1]();
    await settle();
    expect(calls).toEqual([false, true, false]);
    releases[2]();
    await settle();
    stop();
    expect(calls).toEqual([false, true, false]);
  });
});
