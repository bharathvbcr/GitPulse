/**
 * The opt-in wheel diagnostic: what each wheel event carried and what
 * `scrollFeel.ts` took it for, so a person can tell from their own mouse or
 * trackpad whether the glide is being applied to the right device.
 *
 * Off unless `localStorage["gitpulse.debug.wheel"]` is `"1"` when a terminal
 * opens. On, the last `WHEEL_LOG_LIMIT` samples are kept and read with
 * `window.__gitpulseWheelLog()`, and each is also written with
 * `console.debug("[gitpulse-terminal]", "wheel", ...)`, the channel the other
 * terminal diagnostics use (hidden by default in the Web Inspector: choose the
 * Verbose level). See docs/TERMINAL.md.
 */
import { createWheelLog, type WheelSample } from "./scrollFeel";

export const WHEEL_DEBUG_KEY = "gitpulse.debug.wheel";
export const WHEEL_LOG_GLOBAL = "__gitpulseWheelLog";

interface DiagnosticHost {
  storage?: { getItem(key: string): string | null };
  target?: Record<string, unknown>;
  debug?: (...args: unknown[]) => void;
}

/** A sample sink when the flag is on, else undefined. `host` is injectable for tests. */
export function wheelDiagnostic(host: DiagnosticHost = {}): ((sample: WheelSample) => void) | undefined {
  let on = false;
  try {
    const storage = host.storage ?? globalThis.localStorage;
    on = storage?.getItem(WHEEL_DEBUG_KEY) === "1";
  } catch {
    on = false;
  }
  if (!on) return undefined;
  const log = createWheelLog();
  const target = host.target ?? (globalThis as unknown as Record<string, unknown>);
  target[WHEEL_LOG_GLOBAL] = () => log.samples();
  const debug = host.debug ?? ((...args: unknown[]) => console.debug(...args));
  return (sample) => {
    log.push(sample);
    debug("[gitpulse-terminal]", "wheel", `deltaY=${sample.deltaY} wheelDeltaY=${sample.wheelDeltaY} deltaMode=${sample.deltaMode} notch=${sample.notch} gesture=${sample.gestureNotch ? "notch (glide)" : "direct"}`);
  };
}
