/**
 * Wording for build leases and idle sessions in Health (`sysload::lease`,
 * `sysload::activity`). Kept out of the components so how a missing reading
 * reads is tested once: a count that couldn't be read says why, and is never
 * shown as zero.
 */
import { describeReading, formatAge } from "./hostLoad";
import type { IdleReport, IdleSession, LeaseHolder, LeaseStatus, LeaseWaiter } from "../stores/hostLoadStore";

/** "3 of 4 slots free" style line: how many may run now, and why that many. */
export function capacityLine(status: LeaseStatus): string {
  if (!status.on) return "Build leases are switched off in Settings: every build runs at once.";
  const { capacity } = status;
  const base =
    capacity.slots === 0
      ? "No new build may start"
      : `${capacity.slots} of ${status.configured} build slot${status.configured === 1 ? "" : "s"} may run`;
  return capacity.note ? `${base} (${capacity.note}).` : `${base}.`;
}

/** Nanoseconds since a record's grant (or its enqueue), as an age. */
export function recordAge(nowNs: number, grantedNs: number | null, enqueuedNs: number): string {
  const since = grantedNs ?? enqueuedNs;
  return formatAge(Math.max(0, nowNs - since) / 1e6);
}

/** One holder as table cells: slot, build, session, age. */
export function holderCells(holder: LeaseHolder, nowNs: number): string[] {
  const r = holder.record;
  if (!r) return [`${holder.slot}`, `pid ${holder.pid}, unknown`, "unknown", "unknown"];
  return [
    `${holder.slot}`,
    `${r.label} in ${r.cwd}${r.overflow ? " (started without a slot)" : ""}`,
    r.session_key,
    recordAge(nowNs, r.granted_mono_ns, r.enqueued_mono_ns),
  ];
}

/** One waiter as table cells: place, build, session, waiting. */
export function waiterCells(waiter: LeaseWaiter, position: number, nowNs: number): string[] {
  const r = waiter.record;
  return [
    `${position + 1}`,
    `${r.label} in ${r.cwd}`,
    r.session_key,
    recordAge(nowNs, null, r.enqueued_mono_ns),
  ];
}

/** Builds that ran unleased lately, or why that couldn't be counted. */
export function missesLine(status: LeaseStatus): string {
  if (status.misses === null) {
    return `Builds that ran without a lease couldn't be counted: ${status.misses_error ?? "unknown"}.`;
  }
  return status.misses === 0
    ? "No build ran without a lease in the last 15 minutes."
    : `${status.misses} build${status.misses === 1 ? "" : "s"} ran without a lease in the last 15 minutes.`;
}

/** Heavy builds running that hold no slot, or why that couldn't be counted. */
export function unleasedLine(status: LeaseStatus): string {
  if (status.unleased_running === null) {
    return `Running builds that hold no slot couldn't be counted: ${status.unleased_error ?? "unknown"}.`;
  }
  const n = status.unleased_running;
  return n === 0
    ? "Every running heavy build holds a slot."
    : `${n} cargo or go build${n === 1 ? "" : "s"} running hold${n === 1 ? "s" : ""} no slot: ${n === 1 ? "it" : "they"} never went through the lease wrapper.`;
}

/** What one session's activity says, in a sentence. */
export function activityLine(session: IdleSession): string {
  const a = session.activity;
  switch (a.state) {
    case "idle":
      return `Idle for ${formatAge(a.since_ms)}: ${a.why}.`;
    case "busy":
      return `Not idle: ${a.why}.`;
    case "unknown":
      return `Not known to be idle: ${a.why}.`;
  }
}

/** The footprint column for an idle row. */
export function footprintOf(session: IdleSession, format: (n: number) => string): string {
  return describeReading(session.footprint, format);
}

/** Where a terminal tab stands in the build queue: "Waiting for a build slot (2nd)", or null. */
export function waitingLabel(queue: readonly LeaseWaiter[], terminalSession: string): string | null {
  const place = queue.findIndex((w) => w.record.terminal_session === terminalSession);
  if (place < 0) return null;
  const n = place + 1;
  const suffix = n % 100 >= 11 && n % 100 <= 13 ? "th" : (["th", "st", "nd", "rd"][n % 10] ?? "th");
  return `Waiting for a build slot (${n}${n % 10 > 3 ? "th" : suffix})`;
}

/**
 * Whether Health offers Park for a session: it is idle, has been for at least
 * `park_after_s`, and the backend says it can be resumed. The backend checks
 * all of it again before and after its own dialog.
 */
export function parkOffered(session: IdleSession, report: Pick<IdleReport, "park_after_s">): boolean {
  const a = session.activity;
  return (
    a.state === "idle" &&
    a.since_ms >= report.park_after_s * 1000 &&
    session.park_blocked === null &&
    session.terminal_session !== null
  );
}
