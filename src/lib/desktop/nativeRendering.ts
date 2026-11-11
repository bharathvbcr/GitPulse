import { writable, type Readable } from "svelte/store";
import { diagnostics } from "../diagnostics/diagnostics";
import { invoke } from "../ipc/invoke";
import { hostPlatform } from "../stores/platformStore";
import { interfaceStore } from "../stores/interfaceStore";
import { describeFailure } from "../ui/copy/describeFailure";
import type { HostProfile } from "../platform";

/**
 * The two opt-in macOS rendering switches, and the answer for each.
 *
 * `interfaceStore` holds what the reader asked for (`nativeGlass`,
 * `highRefresh`). Whether the window is actually on Liquid Glass, or WebKit is
 * actually rendering above 60 Hz, is a fact only the backend can observe, so
 * every change is sent to `cmd_native_rendering_apply` and what comes back is
 * what the settings page shows. A request the backend could not honor is
 * written to Diagnostics with its reason; it is never shown as on.
 *
 * Mirrors `NativeRenderingStatus` in src-tauri/src/native_rendering.rs.
 */
export interface FlagStatus {
  readonly requested: boolean;
  /** Observed in effect, not echoed from the request. */
  readonly active: boolean;
  /** Present whenever something was requested that is not in effect. */
  readonly reason: string | null;
}

export interface NativeRenderingStatus {
  readonly liquid_glass: FlagStatus;
  readonly high_refresh: FlagStatus;
}

export interface NativeRenderingRequest {
  readonly liquidGlass: boolean;
  readonly highRefresh: boolean;
}

/** The diagnostics ring's source tag for everything this module reports. */
export const NATIVE_RENDERING_SOURCE = "native-rendering";

const NOT_ASKED: FlagStatus = { requested: false, active: false, reason: null };

function parseFlag(value: unknown): FlagStatus | null {
  if (typeof value !== "object" || value === null) return null;
  const raw = value as Record<string, unknown>;
  if (typeof raw.requested !== "boolean" || typeof raw.active !== "boolean") return null;
  if (raw.reason !== null && raw.reason !== undefined && typeof raw.reason !== "string") return null;
  // A flag that claims to be active with a reason attached is contradicting
  // itself; trust the cautious half.
  const reason = typeof raw.reason === "string" && raw.reason.length > 0 ? raw.reason : null;
  return { requested: raw.requested, active: raw.active && reason === null, reason };
}

/** `null` for any reply this build does not recognise: an unreadable answer is not an applied one. */
export function parseNativeRendering(value: unknown): NativeRenderingStatus | null {
  if (typeof value !== "object" || value === null) return null;
  const raw = value as Record<string, unknown>;
  const liquid = parseFlag(raw.liquid_glass);
  const refresh = parseFlag(raw.high_refresh);
  return liquid && refresh ? { liquid_glass: liquid, high_refresh: refresh } : null;
}

/**
 * What to ask the backend for: the stored preference, and only where this build
 * has the code behind it. A host without the capability is never asked, so the
 * stored value of a setting it cannot show stays harmless.
 */
export function requestFor(
  prefs: { readonly nativeGlass: boolean; readonly highRefresh: boolean },
  host: Pick<HostProfile, "liquid_glass" | "high_refresh">,
): NativeRenderingRequest {
  return {
    liquidGlass: prefs.nativeGlass && host.liquid_glass,
    highRefresh: prefs.highRefresh && host.high_refresh,
  };
}

export type NativeRenderingFlag = "liquid_glass" | "high_refresh";

const FLAG_LABEL: Readonly<Record<NativeRenderingFlag, string>> = {
  liquid_glass: "Liquid Glass",
  high_refresh: "120 Hz rendering",
};

/** The diagnostics lines for every switch that was asked for and is not in effect. */
export function refusals(status: NativeRenderingStatus): readonly string[] {
  const lines: string[] = [];
  for (const flag of ["liquid_glass", "high_refresh"] as const) {
    const entry = status[flag];
    if (entry.requested && !entry.active) {
      lines.push(`${FLAG_LABEL[flag]} is on in Settings but not in effect. ${entry.reason ?? "The backend gave no reason."}`);
    }
  }
  return lines;
}

/** What a failed call stands in for: every requested switch refused, with the failure as the reason. */
export function failureStatus(request: NativeRenderingRequest, error: unknown): NativeRenderingStatus {
  const copy = describeFailure({ verb: "apply", object: "the native rendering settings" }, error);
  const reason = copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  const one = (requested: boolean): FlagStatus => (requested ? { requested, active: false, reason } : NOT_ASKED);
  return { liquid_glass: one(request.liquidGlass), high_refresh: one(request.highRefresh) };
}

const current = writable<NativeRenderingStatus | null>(null);

/**
 * The latest answer, or `null` before the first one. `null` is "not asked or not
 * answered yet", which a surface must not draw as "applied".
 */
export const nativeRenderingStatus: Readable<NativeRenderingStatus | null> = { subscribe: current.subscribe };

const boot = writable<NativeRenderingRequest | null>(null);

/**
 * What this launch first asked for, or `null` before it did. WebKit decides the
 * page's frame rate when the page starts: measured in the real app, switching
 * 120 Hz at runtime reports the feature changed while the running page stays at
 * 60 Hz (and the reverse), and a launch with the preference already on runs at
 * 120 Hz. So a 120 Hz change differing from this value waits for the next launch.
 */
export const nativeRenderingBoot: Readable<NativeRenderingRequest | null> = { subscribe: boot.subscribe };

/** Records the launch's first request; `installNativeRendering` is the app's one caller. */
export function setNativeRenderingBoot(request: NativeRenderingRequest | null): void {
  boot.set(request);
}

/** Records an answer for the settings page; `applyNativeRendering` is its one production caller. */
export function publishNativeRenderingStatus(status: NativeRenderingStatus | null): void {
  current.set(status);
}

export interface NativeRenderingDeps {
  readonly apply: (request: NativeRenderingRequest) => Promise<unknown>;
  readonly warn: (source: string, message: string) => void;
  readonly publish: (status: NativeRenderingStatus) => void;
}

const defaultDeps: NativeRenderingDeps = {
  apply: (request) =>
    invoke<unknown>("cmd_native_rendering_apply", {
      liquidGlass: request.liquidGlass,
      highRefresh: request.highRefresh,
    }),
  warn: (source, message) => diagnostics.warn(source, message),
  publish: publishNativeRenderingStatus,
};

/**
 * Sends a request to the backend and reports the outcome. Never rejects.
 * Exported for tests; `installNativeRendering` is the app's one caller.
 */
export async function applyNativeRendering(
  request: NativeRenderingRequest,
  deps: NativeRenderingDeps = defaultDeps,
): Promise<NativeRenderingStatus> {
  let status: NativeRenderingStatus;
  try {
    status = parseNativeRendering(await deps.apply(request)) ?? failureStatus(request, "The window sent back an answer GitPulse couldn't read.");
  } catch (error) {
    status = failureStatus(request, error);
  }
  deps.publish(status);
  for (const line of refusals(status)) deps.warn(NATIVE_RENDERING_SOURCE, line);
  return status;
}

/**
 * Keeps the backend in step with the two preferences. The first request goes out
 * as soon as the host is known to have either switch, even with both off: WebKit
 * keeps the frame-rate feature in the app's user defaults, so a value left by an
 * earlier run (a profile reset, a copied preference) would otherwise outlive the
 * preference that asked for it. A host with neither switch is never asked.
 * Changes are sent one at a time, in order, and only the latest is kept while a
 * call is in flight, so a quick off-on-off leaves the window off.
 *
 * Returns the unsubscribe.
 */
export function installNativeRendering(deps: NativeRenderingDeps = defaultDeps): () => void {
  let host: Pick<HostProfile, "liquid_glass" | "high_refresh"> = { liquid_glass: false, high_refresh: false };
  let prefs = { nativeGlass: false, highRefresh: false };
  let sent: NativeRenderingRequest | null = null;
  let wanted: NativeRenderingRequest | null = null;
  let running = false;

  const drain = async () => {
    if (running) return;
    running = true;
    try {
      while (wanted !== null && (sent === null || wanted.liquidGlass !== sent.liquidGlass || wanted.highRefresh !== sent.highRefresh)) {
        const next: NativeRenderingRequest = wanted;
        if (sent === null) setNativeRenderingBoot(next);
        sent = next;
        await applyNativeRendering(next, deps);
      }
    } finally {
      running = false;
    }
  };

  const update = () => {
    wanted = host.liquid_glass || host.high_refresh ? requestFor(prefs, host) : null;
    void drain();
  };
  // Preferences first: subscribing to the host first would send a request built
  // from the defaults before the stored values were read.
  const stopPrefs = interfaceStore.subscribe((value) => {
    prefs = { nativeGlass: value.nativeGlass, highRefresh: value.highRefresh };
    update();
  });
  const stopHost = hostPlatform.subscribe((value) => {
    host = value;
    update();
  });
  return () => {
    stopHost();
    stopPrefs();
  };
}
