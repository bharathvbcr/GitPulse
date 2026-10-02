/**
 * What a paste sends, and when to ask first.
 *
 * xterm 6.0.0's `paste()` wraps text in the bracketed-paste markers but never
 * removes a marker already inside it, so clipboard text carrying `ESC[201~`
 * ends the bracket early and everything after it runs as typed — the
 * copy-from-a-web-page injection. The markers are removed here, always.
 *
 * Two pastes earn a question. One whose shell is not in bracketed-paste mode
 * and that spans several lines: each line runs the moment it arrives. And one
 * carrying control characters (escape sequences, ^C, ^D), which drive the
 * shell rather than type into it.
 */

export const PASTE_START = "\x1b[200~";
export const PASTE_END = "\x1b[201~";

/** C0 controls and DEL, minus tab and the line endings a paste legitimately carries. */
const CONTROL = /[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g;

export interface PastePlan {
  /** What to hand to `term.paste`. */
  text: string;
  question: { title: string; message: string } | null;
}

/**
 * Removes every bracketed-paste marker, including one the removal itself
 * joins together ("\x1b[20" + marker + "1~"). Each pass shortens the text,
 * so this ends.
 */
export function stripPasteMarkers(text: string): string {
  let current = text;
  for (;;) {
    const next = current.split(PASTE_START).join("").split(PASTE_END).join("");
    if (next === current) return current;
    current = next;
  }
}

function plural(count: number, one: string, many: string): string {
  return `${count} ${count === 1 ? one : many}`;
}

export function planPaste(raw: string, bracketed: boolean): PastePlan {
  const text = stripPasteMarkers(raw);
  const injected = text !== raw;
  const controls = text.match(CONTROL)?.length ?? 0;
  // One trailing newline is "paste and run this command"; that is one line.
  const body = text.replace(/(?:\r\n|\r|\n)$/, "");
  const lines = body === "" ? 0 : body.split(/\r\n|\r|\n/).length;
  if (injected || controls > 0) {
    const parts: string[] = [];
    if (injected) parts.push("It contained a sequence that ends a paste early, which would make the rest run as typed commands; that sequence was removed.");
    if (controls > 0) parts.push(`It holds ${plural(controls, "control character", "control characters")}, which act on the shell rather than type into it.`);
    return {
      text,
      question: { title: "Paste text with control characters?", message: `${parts.join(" ")} Paste only if you trust where this came from.` },
    };
  }
  if (!bracketed && lines > 1) {
    return {
      text,
      question: {
        title: `Paste ${plural(lines, "line", "lines")}?`,
        message: "This shell is not in bracketed-paste mode, so each line runs as a command the moment it arrives.",
      },
    };
  }
  return { text, question: null };
}
