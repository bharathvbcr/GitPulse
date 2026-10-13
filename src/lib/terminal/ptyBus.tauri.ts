import { Channel } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createPtyBus, type OutputMessage, type PtyBus } from "./ptyBus";

/**
 * The one bus every terminal tab shares, bound to Tauri's event transport and
 * its `Channel`.
 *
 * Split from `ptyBus.ts` so the routing rules stay testable in Node: importing
 * `@tauri-apps/api/event` at module scope pulls in the webview IPC, which a
 * unit test has no business standing up just to prove that a chunk reached the
 * right session.
 */
export const ptyBus: PtyBus = createPtyBus(listen, (onmessage) => {
  const channel = new Channel<OutputMessage>();
  channel.onmessage = onmessage;
  return channel;
});
