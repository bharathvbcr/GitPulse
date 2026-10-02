/**
 * Input the guard must never eat. Each case is one an earlier version
 * dropped or doubled, checked against @xterm/xterm 6.0.0's own source:
 *
 * - IME commit with Enter (`compositionend` first): the composed text is
 *   sent by CompositionHelper after the keydown, so the guard must neither
 *   send "\r" nor clear the textarea underneath it.
 * - Dictation and other keyless multi-character inserts: xterm's
 *   `_inputEvent` writes them, because no key is down.
 * - AltGr (Windows Ctrl+Alt, Linux AltGraph) and a pending dead key: xterm
 *   returns from keydown without writing or cancelling, so the keypress is
 *   the only delivery. The guard learns that by watching xterm's keydown
 *   (`observe`), not by predicting it.
 * - An arm whose keyup never comes (Cmd chords, a blur mid-key) expires.
 * - Screen-reader mode: xterm already de-duplicates there, and the guard's
 *   cancellations would hide every keystroke from the reader.
 */
import { describe, expect, it } from "vitest";
import { ARM_TTL_MS, createEraseGuard, type TerminalInputEvent } from "./eraseGuard";

function key(partial: Partial<TerminalInputEvent> & { type: string }): TerminalInputEvent {
  return partial;
}

/**
 * A model of what reaches the PTY. A suppressed event writes nothing; a
 * passed keypress or keyless `insertText` is what xterm writes; a passed
 * keydown writes only when the scenario says xterm wrote it (`xtermWrote`).
 */
function run(
  events: Array<TerminalInputEvent | { observe: boolean } | { expire: true } | { reset: true }>,
  xtermWrote: Map<TerminalInputEvent, string> = new Map(),
): { pty: string; decisions: ReturnType<ReturnType<typeof createEraseGuard>["decide"]>[] } {
  const guard = createEraseGuard();
  let pty = "";
  let last = 0;
  const decisions: ReturnType<typeof guard.decide>[] = [];
  for (const step of events) {
    if ("observe" in step) { guard.observe(step.observe); continue; }
    if ("expire" in step) { guard.expire(last); continue; }
    if ("reset" in step) { guard.reset(); continue; }
    const decision = guard.decide(step);
    decisions.push(decision);
    last = decision.generation;
    if (decision.send) { pty += decision.send; continue; }
    if (decision.suppress) continue;
    if (step.type === "keypress") pty += step.data ?? step.key ?? "";
    else if (step.type === "input" && step.inputType === "insertText") pty += step.data ?? "";
    else if (step.type === "keydown") pty += xtermWrote.get(step) ?? "";
  }
  return { pty, decisions };
}

describe("an IME composition committed with Enter keeps its text", () => {
  for (const [name, commit] of [
    ["Enter", key({ type: "keydown", key: "Enter", keyCode: 229 })],
    ["Tab", key({ type: "keydown", key: "Tab", keyCode: 229 })],
    ["Escape", key({ type: "keydown", key: "Escape", keyCode: 229 })],
  ] as const) {
    it(`does not send ${name} or clear the textarea right after compositionend`, () => {
      const guard = createEraseGuard();
      guard.decide(key({ type: "compositionstart" }));
      guard.decide(key({ type: "compositionupdate", data: "にほんご" }));
      guard.decide(key({ type: "compositionend", data: "日本語" }));
      const decision = guard.decide(commit);
      expect(decision.send).toBeNull();
      expect(decision.suppress).toBe(false);
      expect(decision.clearTextarea).toBe(false);
    });
  }

  it("owns the next keyCode 229 Enter again once the commit key is released", () => {
    const guard = createEraseGuard();
    guard.decide(key({ type: "compositionstart" }));
    guard.decide(key({ type: "compositionend", data: "日本語" }));
    guard.decide(key({ type: "keydown", key: "Enter", keyCode: 229 }));
    guard.decide(key({ type: "keyup", key: "Enter", keyCode: 13 }));
    const next = guard.decide(key({ type: "keydown", key: "Enter", keyCode: 229 }));
    expect(next.send).toBe("\r");
  });
});

describe("keyless inserts reach the shell", () => {
  it("passes dictation and other multi-character insertText when no key is down", () => {
    const { pty } = run([
      key({ type: "beforeinput", inputType: "insertText", data: "hello world" }),
      key({ type: "input", inputType: "insertText", data: "hello world" }),
    ]);
    // beforeinput is not a write in the model; the input event is.
    expect(pty).toBe("hello world");
  });

  it("passes a keyless insert after an earlier key has fully finished", () => {
    const { pty } = run([
      key({ type: "keydown", key: "a", keyCode: 65, timeStamp: 1000 }),
      key({ type: "keyup", key: "a", keyCode: 65, timeStamp: 1040 }),
      { expire: true },
      key({ type: "input", inputType: "insertText", data: "……", timeStamp: 2000 }),
    ]);
    expect(pty).toBe("……");
  });

  it("fails closed when a key was seen and the insert cannot be timed", () => {
    const { pty } = run([
      key({ type: "keydown", key: "a", keyCode: 65 }),
      key({ type: "keyup", key: "a", keyCode: 65 }),
      { expire: true },
      key({ type: "input", inputType: "insertText", data: "the whole line" }),
    ]);
    expect(pty).toBe("");
  });

  it("still refuses a multi-character insert that trails a key by less than the quiet period", () => {
    // A WebKit textarea tail delivered just after keyup is the repeat bug,
    // not dictation. Dictation arrives with no key anywhere near it.
    const { pty } = run([
      key({ type: "keydown", key: "a", keyCode: 65, timeStamp: 1000 }),
      key({ type: "keyup", key: "a", keyCode: 65, timeStamp: 1040 }),
      { expire: true },
      key({ type: "input", inputType: "insertText", data: "the whole line", timeStamp: 1100 }),
      key({ type: "input", inputType: "insertText", data: "dictated words", timeStamp: 5000 }),
    ]);
    expect(pty).toBe("dictated words");
  });

  it("still refuses an autocorrect replacement with no key down", () => {
    const { pty } = run([key({ type: "input", inputType: "insertReplacementText", data: "wired" })]);
    expect(pty).toBe("");
  });
});

describe("characters xterm leaves for keypress are delivered once", () => {
  it("delivers a Windows AltGr character (Ctrl+Alt) from its keypress", () => {
    const { pty } = run([
      key({ type: "keydown", key: "@", keyCode: 81, ctrlKey: true, altKey: true }),
      { observe: false },
      key({ type: "keypress", key: "@", data: "@", ctrlKey: true, altKey: true }),
      key({ type: "input", inputType: "insertText", data: "@" }),
      key({ type: "keyup", key: "@", keyCode: 81 }),
    ]);
    expect(pty).toBe("@");
  });

  it("delivers a Linux AltGr character after an AltGraph keydown", () => {
    const { pty } = run([
      key({ type: "keydown", key: "AltGraph", keyCode: 225 }),
      key({ type: "keydown", key: "@", keyCode: 81 }),
      { observe: false },
      key({ type: "keypress", key: "@", data: "@" }),
      key({ type: "input", inputType: "insertText", data: "@" }),
    ]);
    expect(pty).toBe("@");
  });

  it("delivers a composed dead-key character whose keypress differs from its keydown", () => {
    const { pty } = run([
      key({ type: "keydown", key: "Dead", keyCode: 222 }),
      key({ type: "keyup", key: "Dead", keyCode: 222 }),
      key({ type: "keydown", key: "e", keyCode: 69 }),
      { observe: false },
      key({ type: "keypress", key: "é", data: "é" }),
      key({ type: "input", inputType: "insertText", data: "é" }),
    ]);
    expect(pty).toBe("é");
  });

  it("drops a predicted keypress when xterm did write from keydown", () => {
    // macOS Option+Left: xterm writes ESC[1;3D from keydown and cancels it.
    const left = key({ type: "keydown", key: "ArrowLeft", keyCode: 37, altKey: true });
    const { pty } = run([
      left,
      { observe: true },
      key({ type: "keypress", key: "a", data: "a" }),
      key({ type: "input", inputType: "insertText", data: "a" }),
    ], new Map([[left, "\x1b[1;3D"]]));
    expect(pty).toBe("\x1b[1;3D");
  });

  it("observing a key xterm cancelled changes nothing about a normal letter", () => {
    const a = key({ type: "keydown", key: "a", keyCode: 65 });
    const { pty } = run([
      a,
      { observe: true },
      key({ type: "keypress", key: "a", data: "a" }),
      key({ type: "input", inputType: "insertText", data: "a" }),
    ], new Map([[a, "a"]]));
    expect(pty).toBe("a");
  });

  it("does not let an uncancelled Cmd chord spend the next letter's keypress", () => {
    const b = key({ type: "keydown", key: "b", keyCode: 66 });
    const { pty } = run([
      key({ type: "keydown", key: "c", keyCode: 67, metaKey: true }),
      { observe: false },
      b,
      { observe: true },
      key({ type: "keypress", key: "b", data: "b" }),
    ], new Map([[b, "b"]]));
    expect(pty).toBe("b");
  });
});

describe("an arm always ends", () => {
  it("tells the caller how long an armed key may last", () => {
    const guard = createEraseGuard();
    expect(ARM_TTL_MS).toBeGreaterThan(0);
    expect(ARM_TTL_MS).toBeLessThanOrEqual(2000);
    expect(guard.decide(key({ type: "keydown", key: "a", keyCode: 65 })).armTtlMs).toBe(ARM_TTL_MS);
    expect(guard.decide(key({ type: "keydown", key: "Backspace", keyCode: 8 })).armTtlMs).toBe(ARM_TTL_MS);
    expect(guard.decide(key({ type: "keypress", key: "z", data: "z" })).armTtlMs).toBeNull();
  });

  it("releases on a keyup whose key changed case because Shift was let go first", () => {
    const guard = createEraseGuard();
    guard.decide(key({ type: "keydown", key: "A", keyCode: 65, shiftKey: true, code: "KeyA" }));
    expect(guard.decide(key({ type: "keyup", key: "a", keyCode: 65, code: "KeyA" })).releaseAfterTail).toBe(true);
  });

  it("forgets everything on reset, so an emoji picker opened by a chord still types", () => {
    const { pty } = run([
      key({ type: "keydown", key: " ", keyCode: 32, ctrlKey: true, metaKey: true }),
      { reset: true },
      key({ type: "beforeinput", inputType: "insertText", data: "😀" }),
      key({ type: "input", inputType: "insertText", data: "😀" }),
    ]);
    expect(pty).toBe("😀");
  });
});

describe("screen-reader mode", () => {
  it("never cancels the keypress or input a screen reader announces", () => {
    const guard = createEraseGuard();
    guard.decide(key({ type: "keydown", key: "a", keyCode: 65, screenReader: true }));
    for (const event of [
      key({ type: "keypress", key: "a", data: "a", screenReader: true }),
      key({ type: "beforeinput", inputType: "insertText", data: "a", screenReader: true }),
      key({ type: "input", inputType: "insertText", data: "a", screenReader: true }),
    ]) {
      const decision = guard.decide(event);
      expect(decision.suppress, event.type).toBe(false);
      expect(decision.cancelDefault, event.type).toBe(false);
    }
  });
});

describe("keyCode 229 control punctuation", () => {
  for (const [label, partial, bytes] of [
    ["Ctrl+[", { key: "[", ctrlKey: true }, "\x1b"],
    ["Ctrl+\\", { key: "\\", ctrlKey: true }, "\x1c"],
    ["Ctrl+]", { key: "]", ctrlKey: true }, "\x1d"],
    ["Ctrl+Shift+@", { key: "@", ctrlKey: true, shiftKey: true }, "\0"],
    ["Ctrl+Shift+_", { key: "_", ctrlKey: true, shiftKey: true }, "\x1f"],
  ] as const) {
    it(`sends ${label} the way xterm's keyboard map does`, () => {
      const decision = createEraseGuard().decide(key({ type: "keydown", keyCode: 229, ...partial }));
      expect(decision.send).toBe(bytes);
      expect(decision.suppress).toBe(true);
    });
  }
});
