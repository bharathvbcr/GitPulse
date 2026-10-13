/**
 * Shared display formatters. These are ports of the per-component helpers
 * they replaced; the exact output strings (including thresholds and the
 * empty-string fallbacks) are intentional and covered by format.test.ts.
 */

import type { CommitSignatureCheck } from "./pulse/types";

/** `timestampSec`/`nowSec` are unix epoch SECONDS, matching git timestamps. */
export function formatRelativeTime(timestampSec: number, nowSec?: number): string {
  if (!timestampSec) return "";
  const now = nowSec ?? Math.floor(Date.now() / 1000);
  const diff = Math.max(0, now - timestampSec);
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  if (diff < 2592000) return `${Math.floor(diff / 86400)}d ago`;
  return `${Math.floor(diff / 2592000)}mo ago`;
}

/** Locale date-time for a unix epoch SECONDS timestamp; empty for falsy input. */
export function formatDate(ts: number): string {
  if (!ts) return "";
  return new Date(ts * 1000).toLocaleString();
}

/** Safe truncation for commit hashes; tolerates missing input. */
export function shortHash(hash: string | null | undefined, len = 7): string {
  if (!hash) return "";
  return hash.slice(0, len);
}

/**
 * "1 repository" / "3 repositories" — a count with the right noun form.
 *
 * Canonical for the app: declared once here because two independent copies
 * (fleet/aggregate.ts, repos/wipSummary.ts) had already appeared, and a
 * pluralisation rule spelled twice can be corrected on one side only.
 */
export function plural(count: number, singular: string, pluralForm = `${singular}s`): string {
  return `${count} ${count === 1 ? singular : pluralForm}`;
}

/** Minutes since local midnight as `HH:MM`, the value of an `<input type="time">`. */
export function formatClockMinutes(minutes: number): string {
  return `${String(Math.floor(minutes / 60)).padStart(2, "0")}:${String(minutes % 60).padStart(2, "0")}`;
}

/** Inverse of `formatClockMinutes`; throws on anything but a valid 24-hour `HH:MM`. */
export function parseClockMinutes(clock: string): number {
  if (!/^([01][0-9]|2[0-3]):[0-5][0-9]$/.test(clock)) throw new Error("Choose valid local quiet hours.");
  const [h, m] = clock.split(":").map(Number);
  return h * 60 + m;
}

/** What the details pane says about a commit's signature, and whether it is a clean pass. */
export interface SignatureLabel {
  text: string;
  ok: boolean;
}

/**
 * The label for the one selected commit's signature check.
 *
 * `undefined` is a check still running (the verifier can take seconds) and
 * `null` one that could not be run: neither is "unsigned". A commit that
 * carries a signature is never labelled "Unsigned" because the verifier said
 * `N` or `E`; that means the verifier could not check it (no program, no key,
 * no allowed-signers file), not that nobody signed it.
 */
export function signatureLabel(check: CommitSignatureCheck | null | undefined): SignatureLabel {
  if (check === undefined) return { text: "Checking signature…", ok: false };
  if (check === null) return { text: "Signature not checked", ok: false };
  if (check.signature === "none") return { text: "Unsigned", ok: false };
  if (check.signature === "unknown" && (check.status === "none" || check.status === "cannot_check")) {
    return { text: "Signature state unknown", ok: false };
  }
  switch (check.status) {
    case "good":
      return { text: "Verified", ok: true };
    case "bad":
      return { text: "Bad signature", ok: false };
    case "good_unknown_validity":
      return { text: "Signed, signer not trusted", ok: false };
    case "expired":
      return { text: "Signature expired", ok: false };
    case "expired_key":
      return { text: "Signed with an expired key", ok: false };
    case "revoked":
      return { text: "Signed with a revoked key", ok: false };
    case "cannot_check":
    case "none":
      return { text: "Signed, could not verify", ok: false };
  }
}
