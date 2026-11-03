/**
 * Putting a CI result, or a person's note, back into the agent that did the work.
 *
 * The host decides which way a message can go (`runs.send_to_agent`: a live
 * session's mailbox, a resumed conversation, a managed run's next turn, or none)
 * and sends nothing until a person has seen the exact text and the route and
 * confirmed. This file is the renderer's half: it composes the text from the
 * signal a CI notice carries, holds it to the same bounds the host enforces, and
 * says in words what each route and each outcome means — without rounding a
 * weaker fact up to a stronger one.
 */

import { safeText } from "../terminal/agentPromptText";
import type { AgentSendPreview, AgentSendResult, AttentionSignal } from "./client";

/**
 * The longest message the host accepts, in bytes of UTF-8: `MAX_MESSAGE_BYTES` in
 * `src-tauri/src/agent_mailbox.rs`, which `sendToAgent.test.ts` reads and holds
 * this to. The same text must fit every route, including the 16000-byte prompt
 * argument a resumed conversation takes.
 */
export const AGENT_MESSAGE_MAX_BYTES = 8 * 1024;
/** The longest note a person adds to a composed message, in characters. */
export const AGENT_NOTE_MAX_CHARS = 2000;

const FACT_LABELS: Record<AttentionSignal["kind"], string> = {
  ci_failed: "CI failed on the branch you pushed.",
  ci_cancelled: "CI was cancelled on the branch you pushed.",
  ci_passed: "CI passed on the branch you pushed.",
};

/**
 * The message a CI notice composes: what happened, where, and the run to look
 * at, then the person's own note. Every field the notice carries is producer
 * text (a workflow's name, a branch), so it goes through `safeText`: a control
 * or bidi character in one becomes visible instead of acting on the agent's
 * terminal or reordering the line it sits in.
 */
export function composeAgentMessage(signal: AttentionSignal | undefined, note: string): string {
  const personal = note.trim();
  if (!signal) return personal;
  const lines = [
    FACT_LABELS[signal.kind],
    `Workflow: ${safeText(signal.workflow)}`,
    `Conclusion: ${safeText(signal.conclusion)}`,
    `Branch: ${safeText(signal.branch)}`,
    `Commit: ${signal.head_sha}`,
    ...(signal.url ? [`Run: ${safeText(signal.url)}`] : []),
  ];
  return personal ? `${lines.join("\n")}\n\n${personal}` : lines.join("\n");
}

/** Why `text` cannot be sent, or null. The host checks again; this keeps Confirm honest in the meantime. */
export function messageProblem(text: string): string | null {
  if (!text.trim()) return "Write a note: there is nothing to send yet.";
  const bytes = new TextEncoder().encode(text).length;
  if (bytes > AGENT_MESSAGE_MAX_BYTES) return `This message is ${bytes} bytes; at most ${AGENT_MESSAGE_MAX_BYTES} can be sent. Shorten the note.`;
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/u.test(text)) return "A message cannot contain control characters other than line breaks and tabs.";
  return null;
}

/** A route in words: what it is called and what it will do. */
export function routeTitle(route: AgentSendPreview["route"]): string {
  switch (route) {
    case "mailbox": return "Live session";
    case "resume": return "Resume the conversation";
    case "managed": return "Managed follow-up";
    case "unavailable": return "Cannot be sent";
  }
}

/** The Confirm button for a route. Unavailable has none. */
export function confirmLabel(route: AgentSendPreview["route"]): string | null {
  switch (route) {
    case "mailbox": return "Send to the live session";
    case "resume": return "Resume and send";
    case "managed": return "Send as the next turn";
    case "unavailable": return null;
  }
}

/**
 * What a confirmed send did, said no stronger than it was. `queued` is not
 * delivered, `picked_up` is not read, and a send that was refused says it was
 * not sent and whether sending again can help.
 */
export function describeSend(result: AgentSendResult): { tone: "note" | "error"; text: string } {
  if (result.route === "mailbox") {
    if (result.status === "picked_up") return { tone: "note", text: "Handed to the session's hook, which shows it to the agent. Whether the agent has read it is not known." };
    const until = result.expiresAtMs ? ` It is held until ${new Date(result.expiresAtMs).toLocaleTimeString()}, then dropped.` : "";
    return {
      tone: "note",
      text: result.listening
        ? `Queued. A hook is waiting on the session, so it should be shown to the agent within a moment.${until} Queued is not read.`
        : `Queued for the session. It is shown to the agent the next time its turn ends.${until} Queued is not read.`,
    };
  }
  if (result.route === "managed") {
    if (result.sent) return { tone: "note", text: "Delivered to the managed agent as its next turn. That the turn has begun is shown on the run." };
    const again = result.retryable ? " Nothing was sent; you can send again." : " Nothing was sent.";
    return { tone: "error", text: `${result.reason}${again}` };
  }
  return { tone: "note", text: result.reason };
}

/** What a resumed conversation's start says: the message is its first prompt, and where the tab is. */
export function describeResume(outcome: "started" | "queued" | "opened"): string {
  return outcome === "queued"
    ? "The conversation's checkout is still opening. It resumes with your message as its first prompt as soon as it does."
    : "Resuming the conversation with your message as its first prompt. It is listed under this attempt; Show conversation to watch it.";
}
