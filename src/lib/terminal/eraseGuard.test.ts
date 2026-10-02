import { describe, expect, it } from "vitest";
import {
  applyHelperTextareaHardening,
  createEraseGuard,
  describeTerminalControl,
  eraseSequence,
  isSingleGrapheme,
  webkit229ChordEvent,
  xtermCommitsKeydown,
  type TerminalInputEvent,
} from "./eraseGuard";
import { terminalTabChord } from "./tabs";
import { terminalViewChord } from "./viewControls";

function key(partial: Partial<TerminalInputEvent> & { type: string }): TerminalInputEvent {
  return partial;
}

const backspace = (partial: Partial<TerminalInputEvent> = {}): TerminalInputEvent =>
  key({ type: "keydown", key: "Backspace", keyCode: 8, ...partial });

const del = (partial: Partial<TerminalInputEvent> = {}): TerminalInputEvent =>
  key({ type: "keydown", key: "Delete", keyCode: 46, ...partial });

describe("terminal erase guard", () => {
  it("sends one DEL and swallows the WebKit reinsertion of that key", () => {
    const guard = createEraseGuard();
    const erase = guard.decide(backspace());
    expect(erase.send).toBe("\x7f");
    expect(erase.suppress).toBe(true);
    expect(erase.trace).toBe("erase");
    for (const follow of [
      key({ type: "keypress", key: "Backspace", keyCode: 8, data: "\b" }),
      key({ type: "keypress", keyCode: 127, data: "\x7f" }),
      key({ type: "keypress", data: "z" }),
      key({ type: "beforeinput", inputType: "insertText", data: "z" }),
      key({ type: "input", inputType: "insertText", data: "the whole line" }),
      key({ type: "input", inputType: "insertReplacementText", data: "the whole line" }),
      key({ type: "beforeinput", inputType: "historyUndo", data: "the whole line" }),
      key({ type: "input", inputType: "historyRedo", data: "the whole line" }),
      key({ type: "input", inputType: "insertFromPaste", data: "the whole line" }),
      key({ type: "compositionupdate", data: "the whole line" }),
      key({ type: "compositionend", data: "the whole line" }),
      key({ type: "input", data: "unlabeled" }),
    ]) {
      const decision = guard.decide(follow);
      expect(decision.suppress, follow.inputType ?? follow.type).toBe(true);
      expect(decision.send).toBeNull();
      expect(decision.detail).not.toContain("whole");
      expect(decision.detail).not.toContain("unlabeled");
    }
    expect(guard.decide(key({ type: "input", inputType: "deleteContentBackward" })).suppress).toBe(false);
    expect(guard.decide(key({ type: "beforeinput", inputType: "deleteContentForward" })).suppress).toBe(false);
  });

  it("matches xterm's Backspace and Delete sequences, including every modifier mask", () => {
    expect(eraseSequence("backspace", backspace())).toBe("\x7f");
    expect(eraseSequence("backspace", backspace({ ctrlKey: true }))).toBe("\b");
    expect(eraseSequence("backspace", backspace({ altKey: true }))).toBe("\x1b\x7f");
    expect(eraseSequence("backspace", backspace({ altKey: true, ctrlKey: true }))).toBe("\x1b\b");
    expect(eraseSequence("backspace", backspace({ shiftKey: true, metaKey: true }))).toBe("\x7f");
    for (let mask = 0; mask < 16; mask++) {
      const event = del({
        shiftKey: (mask & 1) !== 0,
        altKey: (mask & 2) !== 0,
        ctrlKey: (mask & 4) !== 0,
        metaKey: (mask & 8) !== 0,
      });
      const expected = mask === 0 ? "\x1b[3~" : `\x1b[3;${mask + 1}~`;
      expect(createEraseGuard().decide(event).send).toBe(expected);
    }
  });

  it("does not treat Ctrl-H as an erase and does not swallow the next letter", () => {
    const guard = createEraseGuard();
    guard.decide(backspace());
    const ctrlH = guard.decide(key({ type: "keydown", key: "h", keyCode: 72, ctrlKey: true }));
    expect(ctrlH.send).toBeNull();
    expect(ctrlH.suppress).toBe(false);
    // xterm already wrote Ctrl-H from keydown. The keypress of that same chord must not write it again.
    expect(guard.decide(key({ type: "keypress", key: "h", data: "h" })).suppress).toBe(true);
    expect(guard.decide(key({ type: "input", inputType: "insertText", data: "h" })).suppress).toBe(true);
    const next = guard.decide(key({ type: "keydown", key: "a", keyCode: 65 }));
    expect(next.suppress).toBe(false);
    expect(next.send).toBeNull();
  });

  it("leaves an IME composition alone, including Backspace used to edit it", () => {
    const composing = createEraseGuard();
    const held = composing.decide(backspace({ isComposing: true, keyCode: 229 }));
    expect(held.send).toBeNull();
    expect(held.suppress).toBe(false);
    expect(composing.decide(key({ type: "input", inputType: "insertCompositionText", data: "あ" })).suppress).toBe(false);

    const started = createEraseGuard();
    started.decide(backspace());
    expect(started.decide(key({ type: "compositionstart" })).suppress).toBe(false);
    expect(started.decide(key({ type: "input", inputType: "insertCompositionText", data: "한" })).suppress).toBe(false);
  });

  it("still owns a non-composing Backspace that WebKit reports as keyCode 229", () => {
    const guard = createEraseGuard();
    const erase = guard.decide(backspace({ keyCode: 229 }));
    expect(erase.send).toBe("\x7f");
    expect(erase.suppress).toBe(true);
    expect(guard.decide(key({ type: "input", inputType: "insertText", data: "line" })).suppress).toBe(true);
  });

  it("disarms on the next real key before that key's keypress, and a modifier does not", () => {
    const guard = createEraseGuard();
    guard.decide(backspace());
    const shift = guard.decide(key({ type: "keydown", key: "Shift", keyCode: 16 }));
    expect(shift.suppress).toBe(false);
    expect(guard.decide(key({ type: "input", inputType: "insertText", data: "line" })).suppress).toBe(true);
    const letter = guard.decide(key({ type: "keydown", key: "a", keyCode: 65 }));
    expect(letter.suppress).toBe(false);
    expect(letter.send).toBeNull();
    // Lowercase is committed on keydown, so the following keypress is the duplicate.
    expect(guard.decide(key({ type: "keypress", key: "a", data: "a" })).suppress).toBe(true);
    expect(guard.decide(key({ type: "beforeinput", inputType: "insertFromPaste", data: "pasted" })).suppress).toBe(false);
  });

  it("keeps suppressing until the keyup generation expires, and an older expiry cannot clear a newer erase", () => {
    const guard = createEraseGuard();
    const first = guard.decide(backspace());
    const up = guard.decide(key({ type: "keyup", key: "Backspace", keyCode: 8 }));
    expect(up.releaseAfterTail).toBe(true);
    expect(up.suppress).toBe(false);
    expect(guard.decide(key({ type: "input", inputType: "insertText", data: "line" })).suppress).toBe(true);
    guard.expire(up.generation);
    // A textarea dump stays suppressed after disarm. A single character proves the arm is gone.
    expect(guard.decide(key({ type: "input", inputType: "insertText", data: "line" })).suppress).toBe(true);
    expect(guard.decide(key({ type: "input", inputType: "insertText", data: "z" })).suppress).toBe(false);

    const older = guard.decide(backspace());
    const newer = guard.decide(backspace({ repeat: true }));
    expect(older.generation).not.toBe(newer.generation);
    guard.expire(older.generation);
    expect(guard.decide(key({ type: "keypress", data: "z" })).suppress).toBe(true);
    guard.expire(newer.generation);
    expect(guard.decide(key({ type: "keypress", data: "z" })).suppress).toBe(false);
    expect(first.generation).toBe(1);
  });

  it("sends one erase per repeat and keeps two sessions independent", () => {
    const first = createEraseGuard();
    const second = createEraseGuard();
    const a = first.decide(backspace({ repeat: true }));
    const b = first.decide(backspace({ repeat: true }));
    expect(a.send).toBe("\x7f");
    expect(b.send).toBe("\x7f");
    expect(b.generation).toBe(a.generation + 1);
    expect(second.decide(key({ type: "input", inputType: "insertText", data: "k" })).suppress).toBe(false);
    expect(second.decide(backspace()).generation).toBe(1);
  });

  it("accepts unidentified key codes, empty events, and a huge insert without leaking text", () => {
    const guard = createEraseGuard();
    expect(guard.decide(key({ type: "keydown", key: "Unidentified", keyCode: 8 })).send).toBe("\x7f");
    expect(guard.decide(key({ type: "" })).suppress).toBe(false);
    expect(guard.decide(key({ type: "pointerdown" })).suppress).toBe(false);
    const secret = "p@ssword-\ud800";
    const huge = `${secret}${"a".repeat(100_000)}`;
    const decision = guard.decide(key({ type: "input", inputType: "insertText", data: huge }));
    expect(decision.suppress).toBe(true);
    expect(decision.detail).toContain(`len=${huge.length}`);
    expect(decision.detail).not.toContain("p@ssword");
    expect(decision.detail).not.toContain("aaa");
    expect(decision.detail).not.toContain("\ud800");
    expect(() => guard.decide(key({ type: "input", inputType: "insertText", data: "\ud800" }))).not.toThrow();
  });

  it("forwards no reinsertion across ten thousand mixed keystrokes", () => {
    const guard = createEraseGuard();
    let erases = 0;
    for (let i = 0; i < 2500; i++) {
      const typed = guard.decide(key({ type: "keydown", key: "q", keyCode: 81 }));
      expect(typed.send).toBeNull();
      expect(typed.suppress).toBe(false);

      const erase = guard.decide(backspace({ repeat: i % 2 === 0 }));
      expect(erase.send).toBe("\x7f");
      expect(erase.suppress).toBe(true);
      erases += 1;

      const press = guard.decide(key({ type: "keypress", data: "q" }));
      expect(press.suppress).toBe(true);
      expect(press.detail).not.toContain("q");

      const line = `secret-line-${i}`;
      const again = guard.decide(key({ type: "input", inputType: "insertReplacementText", data: line }));
      expect(again.suppress).toBe(true);
      expect(again.send).toBeNull();
      expect(again.detail).not.toContain("secret");

      const up = guard.decide(key({ type: "keyup", key: "Backspace", keyCode: 8 }));
      expect(up.releaseAfterTail).toBe(true);
      expect(guard.decide(key({ type: "input", inputType: "insertText", data: line })).suppress).toBe(true);
      guard.expire(up.generation);
      expect(guard.decide(key({ type: "input", inputType: "insertText", data: line })).suppress).toBe(true);
      expect(guard.decide(key({ type: "input", inputType: "insertText", data: "q" })).suppress).toBe(false);

      if (i % 50 === 0) {
        const stale = erase.generation;
        guard.decide(backspace());
        erases += 1;
        guard.expire(stale);
        expect(guard.decide(key({ type: "input", inputType: "insertText", data: "q" })).suppress).toBe(true);
        guard.expire(stale + 1);
        expect(guard.decide(key({ type: "input", inputType: "insertText", data: "q" })).suppress).toBe(false);
      }
    }
    expect(erases).toBe(2500 + Math.ceil(2500 / 50));
  });
});

describe("terminal typing does not repeat", () => {
  const sentence = "When I start typing in the gitpulse terminal, the text repeats itself, it's weird.";

  function accept(pty: string, event: TerminalInputEvent, decision: { send: string | null; suppress: boolean }): string {
    if (decision.send) return pty + decision.send;
    if (decision.suppress) return pty;
    if (event.type === "keypress" || (event.type === "input" && event.inputType === "insertText")) {
      return pty + (event.data ?? "");
    }
    if (event.type === "keydown" && event.key && event.key.length === 1 && !event.ctrlKey && !event.altKey && !event.metaKey) {
      const code = event.key.codePointAt(0) ?? 0;
      const deferred = code >= 65 && code <= 90;
      if (!deferred && event.key !== " " && (event.keyCode ?? 0) >= 48 && (event.keyCode ?? 0) !== 229) {
        return pty + event.key;
      }
    }
    return pty;
  }

  it("writes a lowercase letter once when keypress and the textarea both follow", () => {
    // keyCode 229 never reaches xterm's keydown write. The letter arrives once,
    // as the keypress. keydown's `key` is the wrong case under Caps Lock, so
    // it is not sent from here. The textarea dump is stopped.
    const guard = createEraseGuard();
    let pty = "";
    const down = key({ type: "keydown", key: "a", keyCode: 229 });
    const owned = guard.decide(down);
    pty = accept(pty, down, owned);
    expect(owned.send).toBeNull();
    expect(owned.suppress).toBe(true);
    expect(owned.clearTextarea).toBe(true);
    expect(owned.detail).not.toContain("a");
    const press = key({ type: "keypress", key: "a", data: "a" });
    const pressed = guard.decide(press);
    pty = accept(pty, press, pressed);
    expect(pressed.suppress).toBe(false);
    expect(pressed.send).toBeNull();
    for (const event of [
      key({ type: "input", inputType: "insertText", data: "a" }),
      key({ type: "input", inputType: "insertText", data: "When I start typing" }),
      key({ type: "input", inputType: "insertReplacementText", data: "wired" }),
    ]) {
      const decision = guard.decide(event);
      pty = accept(pty, event, decision);
      expect(decision.suppress, event.inputType ?? event.type).toBe(true);
      expect(decision.send).toBeNull();
      if ((event.data?.length ?? 0) > 1) expect(decision.detail).not.toContain(event.data ?? "");
    }
    expect(pty).toBe("a");
  });

  it("writes a sentence once when WebKit reports keyCode 229 and resends the textarea", () => {
    const guard = createEraseGuard();
    let pty = "";
    for (const ch of sentence) {
      const events = [
        key({ type: "keydown", key: ch, keyCode: 229 }),
        key({ type: "input", inputType: "insertText", data: pty + ch }),
        key({ type: "input", inputType: "insertReplacementText", data: "wired" }),
        key({ type: "keypress", key: ch, data: ch }),
        key({ type: "input", inputType: "insertText", data: ch }),
        key({ type: "keyup", key: ch, keyCode: 229 }),
      ];
      for (const event of events) {
        const decision = guard.decide(event);
        pty = accept(pty, event, decision);
        if ((event.data?.length ?? 0) > 1) expect(decision.detail).not.toContain(event.data ?? "");
        if (decision.releaseAfterTail) guard.expire(decision.generation);
      }
    }
    expect(pty).toBe(sentence);
    expect(pty).not.toContain("WhenWhen");
    expect(pty).not.toContain("repeWhen");
  });

  it("keeps one character per composition key across a long burst, including non-ASCII", () => {
    const guard = createEraseGuard();
    const alphabet = "café 世界 — typing";
    let pty = "";
    for (let round = 0; round < 200; round++) {
      for (const ch of alphabet) {
        const line = pty + ch;
        const events = [
          key({ type: "keydown", key: ch, keyCode: 229, repeat: round % 17 === 0 }),
          key({ type: "input", inputType: "insertText", data: line }),
          key({ type: "input", inputType: "insertReplacementText", data: line + line }),
          key({ type: "keypress", key: ch, data: ch }),
          key({ type: "keyup", key: ch, keyCode: 229 }),
        ];
        for (const event of events) {
          const decision = guard.decide(event);
          pty = accept(pty, event, decision);
          if ((event.data?.length ?? 0) > 1) expect(decision.detail).not.toContain(event.data ?? "");
          if (decision.releaseAfterTail) guard.expire(decision.generation);
        }
      }
    }
    expect(pty).toBe(alphabet.repeat(200));
  });

  it("does not take ownership of a modified composition key or a normal letter", () => {
    const guard = createEraseGuard();
    const chord = guard.decide(key({ type: "keydown", key: "c", keyCode: 229, ctrlKey: true }));
    expect(chord.send).toBe("\u0003");
    expect(chord.suppress).toBe(true);
    expect(guard.decide(key({ type: "keypress", key: "c", data: "c" })).suppress).toBe(true);
    const plain = guard.decide(key({ type: "keydown", key: "c", keyCode: 67 }));
    expect(plain.send).toBeNull();
    expect(plain.suppress).toBe(false);
    // xterm writes the lowercase letter on keydown, so this keypress is the second copy.
    expect(guard.decide(key({ type: "keypress", key: "c", data: "c" })).suppress).toBe(true);
    const upper = guard.decide(key({ type: "keydown", key: "C", keyCode: 65 }));
    expect(upper.send).toBeNull();
    expect(upper.suppress).toBe(false);
    expect(guard.decide(key({ type: "keypress", key: "C", data: "C" })).suppress).toBe(false);
  });

  it("sends one application-cursor arrow for keyCode 229 and swallows the keypress", () => {
    const guard = createEraseGuard();
    const arrow = guard.decide(key({ type: "keydown", key: "ArrowLeft", keyCode: 229, applicationCursor: true }));
    expect(arrow.send).toBe("\x1bOD");
    expect(arrow.suppress).toBe(true);
    expect(arrow.clearTextarea).toBe(true);
    expect(guard.decide(key({ type: "keypress", key: "ArrowLeft" })).suppress).toBe(true);
    const normal = createEraseGuard().decide(key({ type: "keydown", key: "ArrowLeft", keyCode: 229 }));
    expect(normal.send).toBe("\x1b[D");
  });

  it("turns writing tools off on the helper textarea", () => {
    const attrs: Record<string, string> = {};
    const textarea = {
      setAttribute(name: string, value: string) { attrs[name] = value; },
      spellcheck: true,
      autocomplete: "on",
    };
    applyHelperTextareaHardening(textarea);
    expect(attrs.autocomplete).toBe("off");
    expect(attrs.autocorrect).toBe("off");
    expect(attrs.autocapitalize).toBe("off");
    expect(attrs.spellcheck).toBe("false");
    expect(attrs.writingsuggestions).toBe("false");
    expect(attrs["data-gramm"]).toBe("false");
    expect(textarea.spellcheck).toBe(false);
    expect(textarea.autocomplete).toBe("off");
  });

  function drive(events: TerminalInputEvent[]): string {
    const guard = createEraseGuard();
    let pty = "";
    for (const event of events) {
      const decision = guard.decide(event);
      pty = accept(pty, event, decision);
      if (decision.releaseAfterTail) guard.expire(decision.generation);
    }
    return pty;
  }

  it("lets xterm write a capital from keypress, and a fast lowercase does not drop it or double it", () => {
    const upper = key({ type: "keydown", key: "A", keyCode: 65 });
    const lower = key({ type: "keydown", key: "b", keyCode: 66 });
    expect(drive([
      upper,
      key({ type: "keypress", key: "A", data: "A" }),
      key({ type: "input", inputType: "insertText", data: "A" }),
      key({ type: "keyup", key: "A" }),
      lower,
      key({ type: "keypress", key: "b", data: "b" }),
      key({ type: "input", inputType: "insertText", data: "Ab" }),
      key({ type: "keyup", key: "b" }),
    ])).toBe("Ab");
    // Keydowns overlap. "b" is committed at its keydown, so it lands before A's keypress.
    expect(drive([
      upper,
      lower,
      key({ type: "keypress", key: "A", data: "A" }),
      key({ type: "keypress", key: "b", data: "b" }),
      key({ type: "input", inputType: "insertText", data: "bA" }),
      key({ type: "input", inputType: "insertReplacementText", data: "wired" }),
      key({ type: "keyup", key: "A" }),
      key({ type: "keyup", key: "b" }),
    ])).toBe("bA");
  });

  it("writes a space once, and an autocorrect rewrite never replaces the letter", () => {
    expect(drive([
      key({ type: "keydown", key: " ", keyCode: 32 }),
      key({ type: "keypress", key: " ", data: " " }),
      key({ type: "input", inputType: "insertText", data: " " }),
      key({ type: "keyup", key: " " }),
    ])).toBe(" ");
    const guard = createEraseGuard();
    guard.decide(key({ type: "keydown", key: "a", keyCode: 65 }));
    guard.decide(key({ type: "keyup", key: "a" }));
    guard.expire(1);
    const replaced = guard.decide(key({ type: "input", inputType: "insertReplacementText", data: "wired" }));
    expect(replaced.suppress).toBe(true);
    expect(replaced.send).toBeNull();
    expect(replaced.detail).not.toContain("wired");
  });

  it("delivers one emoji grapheme and does not split a family emoji", () => {
    const family = "👨‍👩‍👧‍👦";
    expect(isSingleGrapheme(family)).toBe(true);
    expect(isSingleGrapheme("é")).toBe(true);
    expect(isSingleGrapheme("e\u0301")).toBe(true);
    expect(isSingleGrapheme("ab")).toBe(false);
    expect(isSingleGrapheme("a".repeat(65))).toBe(false);
    expect(() => isSingleGrapheme("\ud800")).not.toThrow();
    expect(drive([
      key({ type: "keydown", key: family, keyCode: 229 }),
      key({ type: "input", inputType: "insertText", data: family + family }),
      key({ type: "keypress", key: family, data: family }),
      key({ type: "input", inputType: "insertReplacementText", data: "wired" }),
      key({ type: "keyup", key: family }),
    ])).toBe(family);
  });

  it("keeps a keyCode 229 chord for the tab strip and the find view", () => {
    const raw = {
      type: "keydown",
      key: "T",
      keyCode: 229,
      ctrlKey: true,
      shiftKey: true,
      altKey: false,
      metaKey: false,
      which: 229,
      defaultPrevented: false,
      stopped: false,
      preventDefault() { this.defaultPrevented = true; },
      stopPropagation() { this.stopped = true; },
    };
    expect(terminalTabChord(raw)).toBeNull();
    const chord = webkit229ChordEvent(raw);
    expect(chord).not.toBeNull();
    expect(terminalTabChord(chord!)).toBe("new");
    expect(chord!.keyCode).toBe(0);
    expect(chord!.which).toBe(0);
    chord!.preventDefault();
    expect(raw.defaultPrevented).toBe(true);
    const find = webkit229ChordEvent({
      type: "keydown",
      key: "f",
      keyCode: 229,
      metaKey: true,
      ctrlKey: false,
      altKey: false,
      shiftKey: false,
    });
    expect(terminalViewChord(find!)).toBe("find");
    const host: {
      type: string; key: string; keyCode: number; metaKey: boolean; ctrlKey: boolean;
      altKey: boolean; shiftKey: boolean; prevented: boolean;
      readonly defaultPrevented?: boolean; preventDefault?: () => void;
    } = {
      type: "keydown",
      key: "f",
      keyCode: 229,
      metaKey: true,
      ctrlKey: false,
      altKey: false,
      shiftKey: false,
      prevented: false,
    };
    Object.defineProperty(host, "defaultPrevented", {
      get() {
        if (this !== host) throw new TypeError("Illegal invocation");
        return host.prevented;
      },
    });
    host.preventDefault = function preventDefault(this: typeof host) {
      if (this !== host) throw new TypeError("Illegal invocation");
      this.prevented = true;
    };
    const hosted = webkit229ChordEvent(host);
    expect(hosted!.defaultPrevented).toBe(false);
    expect(terminalViewChord(hosted!)).toBe("find");
    hosted!.preventDefault!();
    expect(host.prevented).toBe(true);
    expect(webkit229ChordEvent({ type: "keydown", key: "a", keyCode: 229, isComposing: true })).toBeNull();
    expect(webkit229ChordEvent({ type: "keydown", key: "a", keyCode: 65 })).toBeNull();
    expect(webkit229ChordEvent({ type: "keydown", key: "Process", keyCode: 229 })).toBeNull();
  });

  it("does not let a Ctrl chord spend the next letter's keypress", () => {
    expect(xtermCommitsKeydown(key({ type: "keydown", key: "a", keyCode: 65 }))).toBe(true);
    expect(xtermCommitsKeydown(key({ type: "keydown", key: "A", keyCode: 65 }))).toBe(false);
    expect(drive([
      key({ type: "keydown", key: "W", keyCode: 87, ctrlKey: true, shiftKey: true }),
      key({ type: "keydown", key: "a", keyCode: 65 }),
      key({ type: "keypress", key: "a", data: "a" }),
      key({ type: "input", inputType: "insertText", data: "Wa" }),
    ])).toBe("a");
  });

  it("writes ten thousand hostile keystrokes once each", () => {
    const alphabet = "abcdefghijklmnopqrstuvwxyz0123456789 .,";
    let state = 0xc0ffee;
    const rand = () => {
      state = (state + 0x6d2b79f5) >>> 0;
      let t = Math.imul(state ^ (state >>> 15), 1 | state);
      t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
    const codeFor = (ch: string) => {
      if (ch === " ") return 32;
      if (ch === ",") return 188;
      if (ch === ".") return 190;
      if (ch >= "a" && ch <= "z") return ch.charCodeAt(0) - 32;
      return ch.charCodeAt(0);
    };
    const guard = createEraseGuard();
    let pty = "";
    for (let i = 0; i < 10_000; i++) {
      const ch = alphabet[i % alphabet.length];
      const keyCode = rand() < 0.5 ? 229 : codeFor(ch);
      const prior = pty;
      const hostile = [
        key({ type: "beforeinput", inputType: "insertText", data: prior + ch }),
        key({ type: "input", inputType: "insertText", data: prior + ch + prior }),
        key({ type: "input", inputType: "insertReplacementText", data: "wired" }),
        key({ type: "keypress", key: ch, data: ch }),
        key({ type: "input", inputType: "insertText", data: ch }),
        key({ type: "beforeinput", inputType: "insertText", data: ch }),
      ];
      for (let j = hostile.length - 1; j > 0; j--) {
        const swap = Math.floor(rand() * (j + 1));
        const current = hostile[j];
        hostile[j] = hostile[swap];
        hostile[swap] = current;
      }
      const down = key({ type: "keydown", key: ch, keyCode });
      let decision = guard.decide(down);
      pty = accept(pty, down, decision);
      for (const event of hostile) {
        decision = guard.decide(event);
        if (event.inputType === "insertReplacementText") {
          expect(decision.send).toBeNull();
          expect(decision.detail).not.toContain("wired");
        }
        pty = accept(pty, event, decision);
      }
      const up = key({ type: "keyup", key: ch, keyCode });
      decision = guard.decide(up);
      pty = accept(pty, up, decision);
      if (decision.releaseAfterTail) guard.expire(decision.generation);
      expect(pty.length).toBe(i + 1);
      expect(pty[pty.length - 1]).toBe(ch);
    }
    expect(pty).toBe(alphabet.repeat(Math.floor(10_000 / alphabet.length)) + alphabet.slice(0, 10_000 % alphabet.length));
  });
});

describe("terminal control log shape", () => {
  it("describes erases by count and never echoes the typed text", () => {
    expect(describeTerminalControl("")).toBeNull();
    expect(describeTerminalControl("secret")).toBeNull();
    expect(describeTerminalControl("\x1b[3~")).toBeNull();
    expect(describeTerminalControl("\x7f")?.detail).toBe("units=1 erase=1 printable=0 other=0");
    expect(describeTerminalControl("\b")?.detail).toBe("units=1 erase=1 printable=0 other=0");
    const mixed = describeTerminalControl("\x7fsecret\b\x1b");
    expect(mixed?.detail).toBe("units=9 erase=2 printable=6 other=1");
    expect(mixed?.detail).not.toContain("secret");
    const emoji = describeTerminalControl("\x7f🚀");
    expect(emoji?.erase).toBe(1);
    expect(emoji?.printable).toBe(1);
    expect(emoji?.units).toBe(3);
    expect(emoji?.detail).not.toContain("🚀");
  });
});
