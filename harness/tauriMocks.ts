/** What a page's `Channel` looks like to the fixture standing in for the host. */
interface OutputChannel {
  onmessage: (message: ArrayBuffer | string) => void;
}

const outputChannels = new Map<string, OutputChannel>();

/**
 * Records the output channel a session-starting command was handed
 * (`args.onOutput`), under the session id the fixture gives it. The host
 * writes a session's bytes to this channel, not to a process-wide event.
 */
export function adoptOutput(id: string, args: unknown): void {
  const channel: unknown = args && typeof args === "object" ? Reflect.get(args, "onOutput") : undefined;
  if (!channel || typeof channel !== "object" || typeof Reflect.get(channel, "onmessage") !== "function") {
    throw new Error(`fixture: the command that started session ${id} was not handed an output channel`);
  }
  outputChannels.set(id, channel as OutputChannel);
}

/** Bytes handed to each session's channel, for checks that settle credit. */
const pushedTotals = new Map<string, number>();

/** One output message, as the host sends it. False when the session has no channel. */
export function pushOutput(id: string, data: string | Uint8Array): boolean {
  const channel = outputChannels.get(id);
  if (!channel) return false;
  const bytes = typeof data === "string" ? new TextEncoder().encode(data) : data;
  pushedTotals.set(id, (pushedTotals.get(id) ?? 0) + bytes.length);
  channel.onmessage(bytes.slice().buffer);
  return true;
}

/**
 * Every byte pushed to a session so far. The view owes one acknowledgement
 * byte for each, coalesced per frame, so a check that wants to measure one
 * message's repayment waits until the session's acks reach this total first:
 * otherwise credit still owed for earlier output rides in the same ack.
 */
export function pushedBytes(id: string): number {
  return pushedTotals.get(id) ?? 0;
}

/** The marker the host sends after a session's last byte, so its exit may be delivered. */
export function endOutput(id: string): void {
  outputChannels.get(id)?.onmessage("end");
}

/** What the host does when a session ends: the end marker, then the `terminal-exit` event. */
export async function emitExit(id: string, payload: Record<string, unknown>): Promise<void> {
  endOutput(id);
  const { emit } = await import("@tauri-apps/api/event");
  await emit("terminal-exit", { id, ...payload });
}
