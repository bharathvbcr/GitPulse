import { isTauri, type HostOS } from "../platform";

/**
 * Apple Intelligence availability, as the backend reports it.
 *
 * `state` is deliberately a tagged union rather than a boolean plus a message:
 * the four ways this can be unavailable are not interchangeable, and the UI has
 * to be able to tell them apart without parsing prose.
 *
 *   - `not_compiled` — this build has no bridge. A fact about the binary. Says
 *     nothing about the machine, so it must never be shown as a hardware limit.
 *   - `unsupported_os` — not macOS. Nothing the reader can do.
 *   - `unavailable` — the framework answered with a reason. Some of those the
 *     reader can fix (turn Apple Intelligence on) and some resolve themselves
 *     (the model is still downloading).
 *   - `available` — ready now.
 */
export type AppleAvailability =
  | { readonly state: "available" }
  | { readonly state: "not_compiled" }
  | { readonly state: "unsupported_os"; readonly os: string }
  | { readonly state: "unavailable"; readonly reason: string };

export interface AppleIntelligenceStatus {
  readonly available: boolean;
  readonly state: AppleAvailability;
  readonly explanation: string;
}

/**
 * The answer used before the backend replies, and whenever it cannot.
 *
 * Unavailable, always. Offering on-device drafting because a probe failed would
 * put the reader through a generation that cannot run.
 */
export function unknownAppleStatus(os: HostOS): AppleIntelligenceStatus {
  return os === "macos"
    ? {
        available: false,
        state: { state: "unavailable", reason: "not_probed" },
        explanation: "Checking whether Apple Intelligence is available…",
      }
    : {
        available: false,
        state: { state: "unsupported_os", os },
        explanation: `Apple Intelligence is a macOS feature and is not available on ${os}.`,
      };
}

function isAvailabilityState(value: unknown): value is AppleAvailability {
  if (typeof value !== "object" || value === null) return false;
  const state = (value as { state?: unknown }).state;
  return (
    state === "available" ||
    state === "not_compiled" ||
    state === "unsupported_os" ||
    state === "unavailable"
  );
}

/** Parses the `cmd_apple_intelligence_status` envelope, denying on anything odd. */
export function parseAppleStatus(value: unknown, os: HostOS): AppleIntelligenceStatus {
  if (typeof value !== "object" || value === null) return unknownAppleStatus(os);
  const raw = value as Record<string, unknown>;
  if (!isAvailabilityState(raw.state)) return unknownAppleStatus(os);
  const explanation =
    typeof raw.explanation === "string" && raw.explanation.trim().length > 0
      ? raw.explanation
      : unknownAppleStatus(os).explanation;
  // `available` must be exactly `true` AND agree with the tagged state. A
  // mismatch between the two is a backend fault, and the safe reading of a
  // fault is "not available".
  const available = raw.available === true && raw.state.state === "available";
  return { available, state: raw.state, explanation };
}

/**
 * Whether to offer on-device drafting in the UI at all.
 *
 * Hidden rather than disabled on hosts that can never support it: a permanently
 * greyed control invites a reader to hunt for the setting that enables it. On
 * macOS the option is shown even when unavailable, because there the reason is
 * usually actionable — the explanation tells them what to do.
 */
export function showsAppleOption(os: HostOS, status: AppleIntelligenceStatus): boolean {
  if (status.state.state === "unsupported_os") return false;
  if (os !== "macos") return false;
  // A build with no bridge cannot be fixed from the UI, so saying so once in
  // settings is enough; the per-task control stays out of the way.
  return status.state.state !== "not_compiled";
}

/** Reads the live status, or a denying answer if the probe cannot run. */
export async function appleIntelligenceStatus(os: HostOS): Promise<AppleIntelligenceStatus> {
  if (!isTauri()) return unknownAppleStatus(os);
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    return parseAppleStatus(await invoke("cmd_apple_intelligence_status"), os);
  } catch {
    return unknownAppleStatus(os);
  }
}
