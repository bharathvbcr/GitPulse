import { readable, type Readable } from "svelte/store";
import {
  fallbackHostPlatform,
  isTauri,
  parseHostPlatform,
  type HostProfile,
} from "../platform";

/**
 * The host platform and its native capabilities, resolved from the backend.
 *
 * Starts at the webview's best guess so the first paint is not wrong on macOS,
 * then refines once `cmd_host_platform` answers — which is the only source that
 * can distinguish Windows from Linux and can say what actually compiled in.
 *
 * A failed invoke keeps the conservative fallback: no native capability
 * claimed. Read a capability flag to gate a feature; read `os` only to choose
 * the words for something that exists on every platform.
 */
let current: HostProfile = fallbackHostPlatform();
let loaded = false;
let inFlight: Promise<HostProfile> | null = null;
const subscribers = new Set<(value: HostProfile) => void>();

function publish(next: HostProfile): void {
  current = next;
  for (const notify of subscribers) notify(next);
}

/**
 * Loads the platform once per session and caches it.
 *
 * The answer is a property of the running binary, so it cannot change while the
 * app is open; re-invoking per component would be pure overhead. Concurrent
 * callers share one in-flight request.
 */
export async function loadHostPlatform(): Promise<HostProfile> {
  if (loaded) return current;
  if (inFlight) return inFlight;
  if (!isTauri()) {
    loaded = true;
    return current;
  }
  inFlight = (async () => {
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      publish(parseHostPlatform(await invoke("cmd_host_platform")));
    } catch {
      // Keep the conservative fallback. Claiming a capability because the probe
      // failed is the one outcome that must never happen: a check that could
      // not run must not read the same as a check that ran and passed.
    } finally {
      loaded = true;
      inFlight = null;
    }
    return current;
  })();
  return inFlight;
}

/**
 * Read-only view of the resolved profile. Subscribing does NOT invoke.
 *
 * `main.ts` owns the load, deliberately: it fires before anything mounts so a
 * gated control never pops into a panel a moment after it opens. Loading here
 * too would be a second owner for one behaviour — redundant in the app, where
 * the `loaded`/`inFlight` guards make it a no-op, and wrong everywhere else,
 * because it gives every one of the seventeen components that read this store a
 * backend call on mount. The browser harnesses assert that each IPC call they
 * make has an explicit fixture, and this store quietly broke that for all of
 * them. A late subscriber still gets the answer: `publish` notifies the set.
 */
export const hostPlatform: Readable<HostProfile> = readable(current, (set) => {
  set(current);
  subscribers.add(set);
  return () => subscribers.delete(set);
});

/** Synchronous read for non-reactive call sites. */
export function hostPlatformNow(): HostProfile {
  return current;
}

/** Test seam: restores the module to its pre-load state. */
export function resetHostPlatformForTests(next?: HostProfile): void {
  loaded = next !== undefined;
  inFlight = null;
  publish(next ?? fallbackHostPlatform());
}
