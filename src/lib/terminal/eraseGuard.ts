/**
 * One keystroke, one write.
 *
 * WKWebView follows a keydown into xterm's helper textarea with a keypress
 * and an `insertText` / `insertReplacementText`. xterm 6.0.0 already wrote
 * the key from keydown (`CoreBrowserTerminal._keyDown` leaves
 * `_keyDownHandled` false unless screen-reader mode is on, so `_keyPress`
 * writes it again). `CompositionHelper` does worse for keyCode 229: when the
 * textarea changes but keeps its length, `_handleAnyTextareaChanges` writes
 * the entire textarea. That is the line repeating under the cursor, and the
 * autocorrect pass that turns the helper text into a different word.
 *
 * Backspace is the same bug with a deleted glyph. This guard owns that byte
 * (the sequences from `evaluateKeyboardEvent` in @xterm/xterm 6.0.0,
 * Keyboard.ts cases 8 and 46) and stops the keydown before xterm sees it.
 * A generation, not a timer, decides when a later insert is still the tail:
 * the caller expires the generation on keyup, and a newer key is not cleared
 * by the older expiry.
 *
 * `detail` is counts and classes only. Typed text is never copied into it.
 */

export interface TerminalInputEvent {
  type: string;
  key?: string;
  keyCode?: number;
  ctrlKey?: boolean;
  altKey?: boolean;
  metaKey?: boolean;
  shiftKey?: boolean;
  repeat?: boolean;
  isComposing?: boolean;
  inputType?: string;
  /** Set from `term.modes.applicationCursorKeysMode` for a keyCode 229 arrow. */
  applicationCursor?: boolean;
  /** Physical key (`KeyA`). A keyup is matched by it when both sides carry one. */
  code?: string;
  /** `term.options.screenReaderMode`. xterm de-duplicates there itself. */
  screenReader?: boolean;
  /** DOM `timeStamp`, in ms. Absent in tests that do not care about timing. */
  timeStamp?: number;
  /** Never copied into `EraseDecision.detail`. */
  data?: string | null;
}

export interface EraseDecision {
  /** Stop the DOM event before xterm's textarea listeners run. */
  suppress: boolean;
  /**
   * Also cancel the browser default. A keyCode 229 letter keydown stops at
   * xterm but stays uncancelled, so the keypress that carries the real
   * character (Caps Lock included) still arrives.
   */
  cancelDefault: boolean;
  /** Bytes to write once through the existing PTY queue. */
  send: string | null;
  /** Caller expires `generation` after the keyup task, so the textarea tail is still covered. */
  releaseAfterTail: boolean;
  generation: number;
  trace: "erase" | "echo" | "suppress" | null;
  detail: string;
  /** Keydown was stopped before CompositionHelper could snapshot the textarea. */
  clearTextarea: boolean;
  /**
   * Set on every decision that arms: the caller expires `generation` after
   * this many ms even if no keyup comes. macOS sends none for a key pressed
   * under Command, and a blur mid-key loses it too. Null otherwise.
   */
  armTtlMs: number | null;
}

/** Longest an armed key may hold. Key repeat re-arms every ~30-80 ms. */
export const ARM_TTL_MS = 1500;
/**
 * A multi-character insert this soon after a key is a WebKit textarea tail,
 * not dictation. Dictation and the emoji picker arrive with no key near them.
 */
export const KEYLESS_INSERT_QUIET_MS = 250;

export interface ControlShape {
  units: number;
  erase: number;
  printable: number;
  other: number;
  detail: string;
}

const DEL = "\x7f";
const BS = "\b";
const ESC = "\x1b";

const HELPER_TEXTAREA_ATTRIBUTES: Readonly<Record<string, string>> = {
  autocomplete: "off",
  autocapitalize: "off",
  autocorrect: "off",
  spellcheck: "false",
  writingsuggestions: "false",
  "data-gramm": "false",
  "data-enable-grammarly": "false",
};

/**
 * xterm already sets autocorrect and spellcheck. WKWebView still offers
 * writing tools and grammar extensions on the helper textarea; those rewrite
 * the buffer and the rewrite is what gets written to the PTY.
 */
export function applyHelperTextareaHardening(textarea: {
  setAttribute(name: string, value: string): void;
  spellcheck?: boolean;
  autocomplete?: string;
}): void {
  for (const [name, value] of Object.entries(HELPER_TEXTAREA_ATTRIBUTES)) {
    textarea.setAttribute(name, value);
  }
  textarea.autocomplete = "off";
  textarea.spellcheck = false;
}

function none(generation: number): EraseDecision {
  return {
    suppress: false,
    cancelDefault: false,
    send: null,
    releaseAfterTail: false,
    generation,
    trace: null,
    detail: "",
    clearTextarea: false,
    armTtlMs: null,
  };
}

/**
 * A non-IME keyCode 229 never reaches xterm's chord handler, and
 * `isImeComposition` treats 229 as composition. The proxy reports keyCode 0
 * so Ctrl+Shift+T / Ctrl+Tab still match, while preventDefault hits the real event.
 */
export function webkit229ChordEvent<T extends { type?: string; key?: string; keyCode?: number; isComposing?: boolean }>(event: T): T | null {
  if (event.type !== "keydown" || event.keyCode !== 229 || event.isComposing === true) return null;
  if (!event.key || event.key === "Unidentified" || event.key === "Process") return null;
  return new Proxy(event, {
    get(target, prop) {
      if (prop === "keyCode" || prop === "which") return 0;
      // Host getters and methods throw Illegal invocation when `this` is the
      // proxy. Read them against the real event, and bind methods to it too.
      const value = Reflect.get(target, prop, target);
      return typeof value === "function" ? (value as (...args: unknown[]) => unknown).bind(target) : value;
    },
  });
}

/** Backspace or Delete. Prefer `key`; unidentified keys fall back to the legacy code. */
export function eraseKind(event: TerminalInputEvent): "backspace" | "delete" | null {
  if (event.key === "Backspace") return "backspace";
  if (event.key === "Delete") return "delete";
  if (event.key && event.key !== "Unidentified") return null;
  if (event.keyCode === 8) return "backspace";
  if (event.keyCode === 46) return "delete";
  return null;
}

function isModifierOnly(event: TerminalInputEvent): boolean {
  const key = event.key;
  if (key === "Shift" || key === "Control" || key === "Alt" || key === "Meta") return true;
  const code = event.keyCode;
  return code === 16 || code === 17 || code === 18 || code === 91 || code === 93 || code === 224;
}

/**
 * True when @xterm/xterm 6.0.0 writes the key from keydown.
 * Uppercase A-Z, Space, and macOS Option are delivered from keypress instead
 * (`CoreBrowserTerminal._keyDown`). keyCode 229 never reaches that path:
 * `CompositionHelper.keydown` returns false first.
 */
export function xtermCommitsKeydown(event: TerminalInputEvent): boolean {
  if (event.keyCode === 229 || event.isComposing === true) return false;
  if (isModifierOnly(event)) return false;
  const key = event.key ?? "";
  if (!key || key === "Dead" || key === "AltGraph" || key === "Unidentified") return false;
  if (event.metaKey) return false;
  if (event.altKey && !event.ctrlKey) return false;
  if (key === "Enter" || key === "Tab" || key === "Escape") return true;
  if (key.startsWith("Arrow") || key === "Home" || key === "End" || key === "PageUp" || key === "PageDown") return true;
  if (/^F(?:[1-9]|1[0-2])$/.test(key)) return true;
  if (event.ctrlKey && !event.altKey && !event.shiftKey && key.length === 1) return true;
  if (!event.ctrlKey && !event.altKey && !event.metaKey && key.length === 1) {
    const code = key.codePointAt(0) ?? 0;
    if (code >= 65 && code <= 90) return false;
    if (key === " ") return false;
    if ((event.keyCode ?? 0) < 48) return false;
    return true;
  }
  return false;
}

function modifierMask(event: TerminalInputEvent): number {
  return (event.shiftKey ? 1 : 0) | (event.altKey ? 2 : 0) | (event.ctrlKey ? 4 : 0) | (event.metaKey ? 8 : 0);
}

/** xterm 6.0.0 `Keyboard.ts`: Ctrl+[ \ ] are keyCodes 219-221. */
const CTRL_229: Readonly<Record<string, string>> = { "[": ESC, "\\": "\x1c", "]": "\x1d" };
/** xterm 6.0.0 `Keyboard.ts`: `^@` and `^_` are matched by `key` with Ctrl held. */
const CTRL_SHIFT_229: Readonly<Record<string, string>> = { "@": "\0", "_": "\x1f" };

/**
 * Bytes for a non-composing keyCode 229 key that must not reach
 * CompositionHelper. Null means the real character still has to arrive as a
 * keypress or a one-grapheme insert, because keydown's `key` is the wrong
 * case while Caps Lock is on.
 */
function direct229Bytes(event: TerminalInputEvent): string | null {
  if (event.metaKey || event.isComposing) return null;
  const mask = modifierMask(event);
  const modified = mask !== 0;
  const app = event.applicationCursor === true;
  const arrow = (letter: string): string =>
    modified ? `${ESC}[1;${mask + 1}${letter}` : app ? `${ESC}O${letter}` : `${ESC}[${letter}`;
  switch (event.key) {
    case "ArrowLeft": return arrow("D");
    case "ArrowRight": return arrow("C");
    case "ArrowUp": return arrow("A");
    case "ArrowDown": return arrow("B");
    case "Home":
      return modified ? `${ESC}[1;${mask + 1}H` : app ? `${ESC}OH` : `${ESC}[H`;
    case "End":
      return modified ? `${ESC}[1;${mask + 1}F` : app ? `${ESC}OF` : `${ESC}[F`;
    case "PageUp":
      if (event.shiftKey) return null;
      return modified ? `${ESC}[5;${mask + 1}~` : `${ESC}[5~`;
    case "PageDown":
      if (event.shiftKey) return null;
      return modified ? `${ESC}[6;${mask + 1}~` : `${ESC}[6~`;
    case "Enter":
      return event.altKey ? `${ESC}\r` : "\r";
    case "Tab":
      return event.shiftKey ? `${ESC}[Z` : "\t";
    case "Escape":
      return event.altKey ? `${ESC}${ESC}` : ESC;
    default:
      break;
  }
  if (event.ctrlKey && !event.altKey && event.key) {
    const punctuation = event.shiftKey ? CTRL_SHIFT_229[event.key] : CTRL_229[event.key];
    if (punctuation !== undefined) return punctuation;
  }
  if (event.altKey || event.shiftKey || !event.ctrlKey || !event.key || event.key.length !== 1) return null;
  const c = event.key.toLowerCase();
  if (c >= "a" && c <= "z") return String.fromCharCode(c.charCodeAt(0) - 96);
  if (event.key === " ") return "\0";
  return null;
}

/**
 * Matches xterm's Keyboard.ts. Backspace is DEL, or BS when Ctrl is held, with
 * Alt prefixed by ESC. Shift and Meta do not change Backspace. Delete is
 * `ESC [3~`, or `ESC [3;<mask+1>~` where mask is shift + alt*2 + ctrl*4 + meta*8.
 */
export function eraseSequence(kind: "backspace" | "delete", event: TerminalInputEvent): string {
  if (kind === "backspace") {
    const core = event.ctrlKey ? BS : DEL;
    return event.altKey ? ESC + core : core;
  }
  const mask = modifierMask(event);
  return mask === 0 ? `${ESC}[3~` : `${ESC}[3;${mask + 1}~`;
}

/** A textarea edit that would put text back. Deletes are left alone. */
function isReinsertion(inputType: string | undefined): boolean {
  if (!inputType) return true;
  return !inputType.startsWith("delete");
}

function dataLength(data: string | null | undefined): number {
  return typeof data === "string" ? data.length : 0;
}

/** One user-visible character, including an emoji. A textarea dump is longer. */
export function isSingleGrapheme(data: string | null | undefined): boolean {
  if (!data) return false;
  if (data.length === 1) return true;
  if (data.length > 64) return false;
  try {
    const segmenter = new Intl.Segmenter(undefined, { granularity: "grapheme" });
    const iterator = segmenter.segment(data)[Symbol.iterator]();
    const first = iterator.next();
    return !first.done && iterator.next().done === true;
  } catch {
    return false;
  }
}

/** A character the user typed. Control bytes and names (`Backspace`) are not. */
function isTypedGrapheme(data: string | null | undefined): boolean {
  if (!isSingleGrapheme(data)) return false;
  const cp = data!.codePointAt(0) ?? 0;
  return cp >= 0x20 && cp !== 0x7f;
}

function isPayload(event: TerminalInputEvent): boolean {
  if (event.inputType && event.inputType !== "insertText") return false;
  return isTypedGrapheme(event.data);
}

/**
 * xterm left this key for keypress: uppercase A–Z, Space, and macOS Option.
 * Ctrl chords are not in that set — owing a keypress for them would swallow
 * the next real letter.
 */
function deferredToKeypress(event: TerminalInputEvent): boolean {
  if (event.isComposing === true || event.keyCode === 229) return false;
  if (event.metaKey || event.ctrlKey) return false;
  const key = event.key ?? "";
  if (!key || key === "Dead" || key === "AltGraph" || key === "Unidentified") return false;
  if (event.altKey) return true;
  if (key === " ") return true;
  const code = key.codePointAt(0) ?? 0;
  return key.length === 1 && code >= 65 && code <= 90;
}

interface Owed {
  /** `self`: keydown never reached xterm, so the guard writes the character. `xterm`: keypress does. */
  deliver: "self" | "xterm";
  key: string;
  /** The keydown that owes it. `observe` corrects only the latest one. */
  generation: number;
}

/** Same physical key: by `code` when both carry one, else case-insensitively. */
function sameKey(down: { key: string; code: string }, up: TerminalInputEvent): boolean {
  if (down.code && up.code) return down.code === up.code;
  return down.key.toLowerCase() === (up.key ?? "").toLowerCase();
}

export function createEraseGuard() {
  let generation = 0;
  let armed: "erase" | "echo" | null = null;
  let armedKind: "backspace" | "delete" | null = null;
  let armedKey = "";
  let armedCode = "";
  /**
   * A composition just committed. CompositionHelper sends its text after the
   * keydown that ended it (Enter, Tab, Escape, keyCode 229), so that keydown
   * is the IME's, not the shell's. Cleared by the next keyup or real key.
   */
  let compositionTail = false;
  /** Latest keydown xterm saw, for `observe`. Null when the guard owned it. */
  let observable: { generation: number; key: string; candidate: boolean } | null = null;
  /** `timeStamp` of the latest keydown or keyup, when events carry one. */
  let lastKeyAt: number | null = null;
  /** Any non-modifier key has gone down since this guard was made. */
  let keySeen = false;
  /**
   * Keys whose character is not written yet. A later keydown must not drop
   * these: uppercase then a fast lowercase otherwise loses the capital.
   * FIFO matches the browser, which delivers keypresses in keydown order.
   */
  let owed: Owed[] = [];

  /**
   * Whether a multi-character insert may be a key's textarea tail. Fails
   * closed: once any key has been seen, only timestamps that prove the insert
   * is well clear of it let it through.
   */
  function trailsAKey(event: TerminalInputEvent): boolean {
    if (!keySeen) return false;
    if (lastKeyAt === null || typeof event.timeStamp !== "number" || !Number.isFinite(event.timeStamp)) return true;
    return event.timeStamp - lastKeyAt < KEYLESS_INSERT_QUIET_MS;
  }

  function disarm(): void {
    armed = null;
    armedKind = null;
    armedKey = "";
    armedCode = "";
    owed = [];
  }

  function suppress(event: TerminalInputEvent): EraseDecision {
    const inputType = event.inputType ?? "";
    return {
      suppress: true,
      cancelDefault: true,
      send: null,
      releaseAfterTail: false,
      generation,
      trace: "suppress",
      detail: `suppress type=${event.type} inputType=${inputType} len=${dataLength(event.data)} gen=${generation}`,
      clearTextarea: false,
      armTtlMs: null,
    };
  }

  function echoSend(data: string): EraseDecision {
    return {
      suppress: true,
      cancelDefault: true,
      send: data,
      releaseAfterTail: false,
      generation,
      trace: "echo",
      detail: `echo units=${dataLength(data)} gen=${generation}`,
      clearTextarea: false,
      armTtlMs: null,
    };
  }

  function armEcho(
    event: TerminalInputEvent,
    send: string | null,
    suppressKeydown: boolean,
    cancelDefault: boolean,
    trace: "echo" | "suppress" | null,
    deliver: "self" | "xterm" | null,
  ): EraseDecision {
    generation += 1;
    armed = "echo";
    armedKind = null;
    armedKey = event.key ?? "";
    armedCode = event.code ?? "";
    if (deliver) owed.push({ deliver, key: event.key ?? "", generation });
    // A keydown the guard stopped never reaches xterm, so there is nothing
    // to observe. Any other one xterm may write, cancel, or leave to keypress.
    observable = suppressKeydown ? null : {
      generation,
      key: event.key ?? "",
      // Only a key that can still produce a character: not a Command chord,
      // not a plain Ctrl chord. Ctrl+Alt is AltGr on Windows and is one.
      candidate: !event.metaKey && (!event.ctrlKey || event.altKey === true),
    };
    const detail = trace === "echo" && send
      ? `echo units=${dataLength(send)} gen=${generation}`
      : trace === "suppress"
        ? `block keyCode=229 gen=${generation}`
        : "";
    return {
      suppress: suppressKeydown,
      cancelDefault: suppressKeydown && cancelDefault,
      send,
      releaseAfterTail: false,
      generation,
      trace,
      detail,
      clearTextarea: suppressKeydown,
      armTtlMs: ARM_TTL_MS,
    };
  }

  function release(): EraseDecision {
    owed = [];
    return {
      suppress: false,
      cancelDefault: false,
      send: null,
      releaseAfterTail: true,
      generation,
      trace: null,
      detail: "",
      clearTextarea: false,
      armTtlMs: null,
    };
  }

  return {
    decide(event: TerminalInputEvent): EraseDecision {
      const type = event?.type ?? "";
      if (type === "keydown" || type === "keyup") {
        if (!isModifierOnly(event)) {
          keySeen = true;
          lastKeyAt = typeof event.timeStamp === "number" && Number.isFinite(event.timeStamp) ? event.timeStamp : null;
        }
      }
      if (type === "compositionstart" || (type === "keydown" && event.isComposing === true)) {
        disarm();
        compositionTail = false;
        observable = null;
        return none(generation);
      }
      if (type === "compositionend") compositionTail = true;
      if (type === "keydown") {
        if (isModifierOnly(event)) return none(generation);
        if (compositionTail && event.keyCode === 229) {
          // The IME's own commit key. Sending it, or clearing the textarea
          // CompositionHelper is about to read, loses the composed text.
          observable = null;
          return none(generation);
        }
        compositionTail = false;
        if (event.key === "Dead" || event.key === "AltGraph") {
          disarm();
          return none(generation);
        }
        const kind = eraseKind(event);
        if (kind) {
          generation += 1;
          armed = "erase";
          armedKind = kind;
          armedKey = event.key ?? "";
          armedCode = event.code ?? "";
          observable = null;
          return {
            suppress: true,
            cancelDefault: true,
            send: eraseSequence(kind, event),
            releaseAfterTail: false,
            generation,
            trace: "erase",
            detail: `erase key=${kind} code=${event.keyCode ?? 0} gen=${generation} repeat=${event.repeat === true} alt=${event.altKey === true} ctrl=${event.ctrlKey === true}`,
            clearTextarea: true,
            armTtlMs: ARM_TTL_MS,
          };
        }
        if (event.keyCode === 229) {
          const direct = direct229Bytes(event);
          if (direct !== null) return armEcho(event, direct, true, true, "echo", null);
          // Stop CompositionHelper. Leave the default alive so keypress still
          // carries the character Caps Lock actually produced.
          return armEcho(event, null, true, false, "suppress", "self");
        }
        if (event.metaKey || xtermCommitsKeydown(event)) return armEcho(event, null, false, false, null, null);
        if (deferredToKeypress(event)) return armEcho(event, null, false, false, null, "xterm");
        return armEcho(event, null, false, false, null, null);
      }
      if (type === "keyup") {
        if (isModifierOnly(event)) return none(generation);
        compositionTail = false;
        const kind = eraseKind(event);
        if (armed === "erase" && kind !== null && kind === armedKind) return release();
        if (armed === "echo" && sameKey({ key: armedKey, code: armedCode }, event)) return release();
        return none(generation);
      }
      if (event.screenReader === true && type !== "keydown") {
        // xterm marks the keydown handled and skips the keypress and insert
        // itself. Cancelling them here only hides the key from the reader.
        return none(generation);
      }
      if (type === "keypress") {
        const character = isTypedGrapheme(event.key) ? event.key! : isTypedGrapheme(event.data) ? event.data! : "";
        const altGr = event.ctrlKey === true && event.altKey === true && event.metaKey !== true;
        if (!character || event.metaKey || (event.ctrlKey && !altGr)) return armed ? suppress(event) : none(generation);
        const slot = owed[0];
        // keypress carries the real character in charCode. Passing it through
        // lets xterm write once. Sending it here as well would type it twice.
        if (slot?.deliver === "self" || slot?.deliver === "xterm") {
          owed.shift();
          return none(generation);
        }
        if (armed) return suppress(event);
        return none(generation);
      }
      if (type === "beforeinput" || type === "input" || type === "compositionupdate" || type === "compositionend") {
        if (armed === "echo" && (event.inputType === "insertFromPaste" || event.inputType?.startsWith("delete"))) {
          return none(generation);
        }
        if ((type === "beforeinput" || type === "input") && owed[0]?.deliver === "self" && isPayload(event)) {
          owed.shift();
          return echoSend(event.data ?? "");
        }
        if (armed && (type === "compositionupdate" || type === "compositionend" || isReinsertion(event.inputType))) {
          return suppress(event);
        }
        if (!armed && event.inputType === "insertReplacementText") return suppress(event);
        // A keyless multi-character insert is dictation, the emoji picker, or
        // an accessibility tool, and xterm's `_inputEvent` writes it. One that
        // trails a key closely is the WebKit textarea tail instead.
        if (!armed && (type === "beforeinput" || type === "input") && event.inputType === "insertText"
          && !isSingleGrapheme(event.data) && trailsAKey(event)) {
          return suppress(event);
        }
        return none(generation);
      }
      return none(generation);
    },
    /** Drop the arm only when `gen` is still the latest key. */
    expire(gen: number): void {
      if (gen === generation) disarm();
    },
    /**
     * What xterm did with the latest keydown the guard let through, read
     * after xterm's own listener: `cancelled` is its `defaultPrevented`.
     * xterm cancels exactly the keys it wrote. One it left alone — AltGr, a
     * pending dead key, a key it defers — is delivered by the keypress, so
     * the keypress is owed; one it wrote owes nothing, whatever was predicted.
     */
    observe(cancelled: boolean): void {
      const seen = observable;
      observable = null;
      if (!seen || seen.generation !== generation) return;
      const index = owed.findIndex((slot) => slot.generation === seen.generation && slot.deliver === "xterm");
      if (cancelled) {
        if (index >= 0) owed.splice(index, 1);
      } else if (index < 0 && seen.candidate) {
        owed.push({ deliver: "xterm", key: seen.key, generation: seen.generation });
      }
    },
    /** Focus left the terminal: no keyup is coming for anything held. */
    reset(): void {
      disarm();
      compositionTail = false;
      observable = null;
    },
  };
}

/**
 * Shape of a PTY input chunk that contains an erase. Returns null when the
 * chunk has no BS/DEL, so ordinary typing is not logged. The detail string
 * never includes the characters themselves.
 */
export function describeTerminalControl(data: string): ControlShape | null {
  if (!data) return null;
  let erase = 0;
  let printable = 0;
  let other = 0;
  for (const ch of data) {
    const cp = ch.codePointAt(0) ?? 0;
    if (cp === 0x08 || cp === 0x7f) erase += 1;
    else if (cp >= 0x20) printable += 1;
    else other += 1;
  }
  if (erase === 0) return null;
  const shape = { units: data.length, erase, printable, other, detail: "" };
  shape.detail = `units=${shape.units} erase=${shape.erase} printable=${shape.printable} other=${shape.other}`;
  return shape;
}
