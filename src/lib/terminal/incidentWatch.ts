import { toastStore } from "../stores/toastStore";
import { formatError } from "../ui/formatError";
import { hostedClaude, resumers } from "./hostedClaude";
import { INCIDENT_EVENT, createIncidentController } from "./incident";
import type { EventListen } from "./ptyBus";

/**
 * Subscribes to `terminal-incident` and puts each incident in front of the
 * person: what ended the sessions, who sent it when that is known, and one
 * "Resume N" for the Claude conversations that can be picked back up.
 *
 * The toast waits to be dismissed (duration 0): this is the one chance to
 * offer the resume, and a countdown would take the offer away with it.
 * Returns the unsubscribe.
 */
export function watchIncidents(listen: EventListen): Promise<() => void> {
  const controller = createIncidentController({
    store: hostedClaude,
    resumers,
    notify: ({ message, action }) => {
      toastStore.warning(message, action ?? undefined, 0);
    },
    fail: (_terminalId, cause) => {
      toastStore.error(`A Claude conversation could not be resumed: ${formatError(cause)}`);
    },
    done: (started, offered) => {
      if (started < offered) toastStore.info(`Resumed ${started} of ${offered} Claude conversations; the rest have no tab left to resume in.`);
    },
  });
  return listen<unknown>(INCIDENT_EVENT, (event) => {
    controller.handle(event.payload);
  });
}
