/**
 * Model-based fuzz of the input guard against the worst engine it may sit in.
 *
 * The model is deliberately pessimistic: every event the guard lets through
 * is written by xterm — every keypress, every insertText, every
 * compositionend — the way WKWebView has been seen to deliver them, on top of
 * what xterm 6.0.0 writes from keydown. Under that model, the guard alone is
 * what keeps the shell from receiving a character twice or not at all.
 *
 * Each intent is one thing a person does: type a letter, type a capital, hit
 * Backspace, type an AltGr character, commit an IME composition with Enter,
 * dictate a phrase. Intents are interleaved with the noise WebKit adds
 * (duplicate keypresses, textarea tails, autocorrect rewrites) in a seeded
 * random order. The shell must end up with exactly the intended bytes.
 */
import { describe, expect, it } from "vitest";
import { ARM_TTL_MS, KEYLESS_INSERT_QUIET_MS, createEraseGuard, type TerminalInputEvent } from "./eraseGuard";

function rng(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = Math.imul(state ^ (state >>> 15), 1 | state);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

type Step =
  | { event: TerminalInputEvent; xtermKeydown?: { writes: string; cancels: boolean } }
  | { expireTail: true }
  | { reset: true };

interface Intent { expected: string; steps: Step[] }

const LOWER = "abcdefghijklmnopqrstuvwxyz0123456789,./;";
const UPPER = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";

function keyCodeOf(ch: string): number {
  if (ch >= "a" && ch <= "z") return ch.charCodeAt(0) - 32;
  if (ch >= "A" && ch <= "Z") return ch.charCodeAt(0);
  if (ch >= "0" && ch <= "9") return ch.charCodeAt(0);
  return { ",": 188, ".": 190, "/": 191, ";": 186, " ": 32 }[ch] ?? 0;
}

function generate(seed: number, count: number): { intents: Intent[]; expected: string } {
  const random = rng(seed);
  const pick = <T,>(items: readonly T[]) => items[Math.floor(random() * items.length)];
  let clock = 1000;
  const at = (gap = 5 + Math.floor(random() * 20)) => (clock += gap);
  const ev = (partial: Partial<TerminalInputEvent> & { type: string }): TerminalInputEvent => ({ timeStamp: at(), ...partial });
  /** WebKit noise inside one keystroke, before its keyup. */
  const noise = (ch: string): Step[] => {
    const extra: Step[] = [];
    if (random() < 0.5) extra.push({ event: ev({ type: "keypress", key: ch, data: ch }) });
    if (random() < 0.5) extra.push({ event: ev({ type: "input", inputType: "insertText", data: ch }) });
    if (random() < 0.4) extra.push({ event: ev({ type: "input", inputType: "insertText", data: `tail ${ch} tail` }) });
    if (random() < 0.3) extra.push({ event: ev({ type: "input", inputType: "insertReplacementText", data: "wired" }) });
    return extra;
  };
  const release = (key: string, keyCode: number): Step[] => [
    { event: ev({ type: "keyup", key, keyCode }) },
    { expireTail: true },
  ];

  const kinds = ["lower", "upper", "space", "k229", "backspace", "altgr", "ime", "dictation", "blurMidKey"] as const;
  const intents: Intent[] = [];
  for (let i = 0; i < count; i++) {
    const kind = pick(kinds);
    if (kind === "lower") {
      const ch = pick([...LOWER]);
      const code = keyCodeOf(ch);
      intents.push({ expected: ch, steps: [
        { event: ev({ type: "keydown", key: ch, keyCode: code }), xtermKeydown: { writes: ch, cancels: true } },
        ...noise(ch), ...release(ch, code),
      ] });
    } else if (kind === "upper" || kind === "space") {
      const ch = kind === "space" ? " " : pick([...UPPER]);
      const code = keyCodeOf(ch);
      intents.push({ expected: ch, steps: [
        { event: ev({ type: "keydown", key: ch, keyCode: code, shiftKey: kind === "upper" }), xtermKeydown: { writes: "", cancels: false } },
        // xterm's own delivery for these is the keypress.
        { event: ev({ type: "keypress", key: ch, data: ch }) },
        ...noise(ch), ...release(ch, code),
      ] });
    } else if (kind === "k229") {
      const ch = pick([...LOWER.slice(0, 26)]);
      intents.push({ expected: ch, steps: [
        { event: ev({ type: "keydown", key: ch, keyCode: 229 }) },
        { event: ev({ type: "input", inputType: "insertText", data: `${ch}${ch}${ch}` }) },
        { event: ev({ type: "keypress", key: ch, data: ch }) },
        ...noise(ch), ...release(ch, 229),
      ] });
    } else if (kind === "backspace") {
      intents.push({ expected: "\x7f", steps: [
        { event: ev({ type: "keydown", key: "Backspace", keyCode: 8 }) },
        { event: ev({ type: "keypress", key: "Backspace", data: "\b" }) },
        { event: ev({ type: "input", inputType: "insertText", data: "the deleted line" }) },
        ...release("Backspace", 8),
      ] });
    } else if (kind === "altgr") {
      const ch = pick(["@", "{", "[", "\\", "€"]);
      intents.push({ expected: ch, steps: [
        { event: ev({ type: "keydown", key: ch, keyCode: 81, ctrlKey: true, altKey: true }), xtermKeydown: { writes: "", cancels: false } },
        { event: ev({ type: "keypress", key: ch, data: ch, ctrlKey: true, altKey: true }) },
        { event: ev({ type: "input", inputType: "insertText", data: ch }) },
        ...release(ch, 81),
      ] });
    } else if (kind === "ime") {
      const text = pick(["日本語", "中文", "한국어", "é"]);
      intents.push({ expected: text, steps: [
        { event: ev({ type: "compositionstart" }) },
        { event: ev({ type: "keydown", key: "Process", keyCode: 229, isComposing: true }) },
        { event: ev({ type: "compositionupdate", data: text }) },
        { event: ev({ type: "compositionend", data: text }) },
        // The commit key belongs to the IME: it must write nothing.
        { event: ev({ type: "keydown", key: "Enter", keyCode: 229 }) },
        ...release("Enter", 13),
      ] });
    } else if (kind === "dictation") {
      const phrase = pick(["hello world", "git status", "make it so"]);
      at(KEYLESS_INSERT_QUIET_MS + 50);
      intents.push({ expected: phrase, steps: [
        { event: ev({ type: "beforeinput", inputType: "insertText", data: phrase }) },
      ] });
    } else {
      // A key whose keyup never comes, then focus leaves and returns.
      const ch = pick([...LOWER.slice(0, 26)]);
      intents.push({ expected: ch, steps: [
        { event: ev({ type: "keydown", key: ch, keyCode: keyCodeOf(ch) }), xtermKeydown: { writes: ch, cancels: true } },
        ...noise(ch),
        { reset: true },
      ] });
    }
  }
  return { intents, expected: intents.map((intent) => intent.expected).join("") };
}

/** Replays the intents through one guard under the pessimistic engine model. */
function replay(intents: Intent[]): string {
  const guard = createEraseGuard();
  let pty = "";
  let lastGeneration = 0;
  for (const intent of intents) {
    for (const step of intent.steps) {
      if ("expireTail" in step) { guard.expire(lastGeneration); continue; }
      if ("reset" in step) { guard.reset(); continue; }
      const { event } = step;
      const decision = guard.decide(event);
      lastGeneration = decision.generation;
      expect(decision.detail).not.toContain("deleted line");
      expect(decision.detail).not.toContain("tail");
      if (decision.armTtlMs !== null) expect(decision.armTtlMs).toBe(ARM_TTL_MS);
      if (decision.send) { pty += decision.send; continue; }
      if (decision.suppress) continue;
      if (event.type === "keydown") {
        if (step.xtermKeydown) {
          pty += step.xtermKeydown.writes;
          guard.observe(step.xtermKeydown.cancels);
        }
        continue;
      }
      if (event.type === "keypress") pty += event.data ?? event.key ?? "";
      else if ((event.type === "input" || event.type === "beforeinput") && event.inputType === "insertText") {
        // A passed beforeinput is followed by the insert xterm writes; count it once.
        if (event.type === "beforeinput") pty += event.data ?? "";
        else pty += event.data ?? "";
      } else if (event.type === "compositionend") pty += event.data ?? "";
    }
  }
  return pty;
}

describe("the input guard under a pessimistic engine", () => {
  for (const seed of [1, 7, 42, 0xc0ffee, 0xdeadbeef]) {
    it(`writes 4000 mixed intents exactly once each (seed ${seed})`, () => {
      const { intents, expected } = generate(seed, 4000);
      const pty = replay(intents);
      if (pty !== expected) {
        // Name the first divergence instead of dumping two long strings.
        let index = 0;
        while (index < pty.length && pty[index] === expected[index]) index++;
        const intentAt = (() => {
          let offset = 0;
          for (const [n, intent] of intents.entries()) {
            offset += intent.expected.length;
            if (offset > index) return { n, kind: JSON.stringify(intent.steps[0]).slice(0, 160) };
          }
          return null;
        })();
        expect.fail(`diverged at byte ${index} (intent ${JSON.stringify(intentAt)}): got ${JSON.stringify(pty.slice(index, index + 20))}, wanted ${JSON.stringify(expected.slice(index, index + 20))}`);
      }
      expect(pty).toBe(expected);
    });
  }
});
