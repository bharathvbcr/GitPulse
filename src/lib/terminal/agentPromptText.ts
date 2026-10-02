/** The UTF-8 byte ceiling `agentPromptArgs` enforces on an initial agent prompt. */
export const AGENT_PROMPT_MAX_BYTES = 16000;

/** Disclosure appended when a prompt's context, not its instructions, was clipped. */
export const AGENT_CONTEXT_CLIP_NOTE =
  "\n\n[GitPulse context clipped; inspect the repository for the remaining details.]";

const BIDI_CONTROLS = [0x202a, 0x202b, 0x202c, 0x202d, 0x202e, 0x2066, 0x2067, 0x2068, 0x2069];

/**
 * One line of producer-owned text. Paths and labels are rendered as a single
 * line; embedded controls stay visible instead of letting a crafted filename
 * inject a new heading, runnable command, or terminal control into a prompt.
 * C1 controls (NEL, the 8-bit CSI) and the Unicode line and paragraph
 * separators count: terminals and renderers treat them as breaks and escapes.
 */
export function safeText(value: unknown): string {
  if (typeof value !== "string") return "";
  return Array.from(value, (char) => {
    const code = char.codePointAt(0) ?? 0;
    if (char === "\n") return "\\n";
    if (char === "\r") return "\\r";
    if (char === "\t") return "\\t";
    if (BIDI_CONTROLS.includes(code) || code === 0x2028 || code === 0x2029) {
      return `\\u{${code.toString(16)}}`;
    }
    if (code < 0x20 || (code >= 0x7f && code <= 0x9f)) return `\\u{${code.toString(16).padStart(4, "0")}}`;
    return char;
  }).join("");
}

/** A multi-line block of producer-owned text: line breaks and tabs kept, every other control visible. */
export function safeBlock(value: string): string {
  return value.split("\n").map((line) => line.split("\t").map(safeText).join("\t")).join("\n");
}

/** The longest prefix of `value` within `maxBytes` of UTF-8, never splitting a code point. */
function utf8Prefix(value: string, maxBytes: number): string {
  const encoder = new TextEncoder();
  let bytes = 0;
  let text = "";
  for (const char of value) {
    const width = encoder.encode(char).byteLength;
    if (bytes + width > maxBytes) break;
    text += char;
    bytes += width;
  }
  return text;
}

/**
 * Clips to `maxBytes` of UTF-8 without splitting a code point, ending with
 * `note` when it clips. The result never exceeds `maxBytes`: a note that does
 * not fit is itself clipped.
 */
export function clipUtf8(value: string, maxBytes: number, note: string): { text: string; clipped: boolean } {
  const encoder = new TextEncoder();
  const limit = Math.max(0, maxBytes);
  if (encoder.encode(value).byteLength <= limit) return { text: value, clipped: false };
  const fittedNote = utf8Prefix(note, limit);
  const contentLimit = limit - encoder.encode(fittedNote).byteLength;
  return { text: utf8Prefix(value, contentLimit) + fittedNote, clipped: true };
}

/**
 * Instructions followed by context, bounded under the launch ceiling. The
 * instructions are always kept whole; only the context is clipped, and a
 * clip is disclosed by `clipNote`. Instructions that alone exceed the
 * ceiling are a programming error and throw, rather than producing a prompt
 * every launcher would refuse.
 */
export function boundAgentPrompt(instructions: string, context: string, clipNote: string): string {
  const budget = AGENT_PROMPT_MAX_BYTES - new TextEncoder().encode(instructions + "\n\n").length;
  if (budget < 0) {
    throw new Error(`Agent prompt instructions exceed the ${AGENT_PROMPT_MAX_BYTES}-byte launch ceiling`);
  }
  return `${instructions}\n\n${clipUtf8(context, budget, clipNote).text}`;
}
