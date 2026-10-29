import { describe, expect, it, vi } from "vitest";
import { createAckCoalescer, createOutputCredit, OUTPUT_CREDIT_WINDOW } from "./outputCredit";
import {
  createSyncFrameWriter,
  SYNC_HOLD_DEADLINE_MS,
  SYNC_HOLD_MAX_BYTES,
  type SyncFrameScheduler,
  type SyncFrameWriter,
} from "./syncFrames";

const ESC = "\x1b";
const enc = (text: string): Uint8Array => new TextEncoder().encode(text);
const dec = (bytes: Uint8Array): string => new TextDecoder().decode(bytes);
const C1_CSI = "\u009b"; // UTF-8 C2 9B: what xterm's decoder turns into the 8-bit CSI

/** A scheduler the test advances by hand. */
function fakeClock() {
  let now = 0;
  const timers = new Map<number, { at: number; fn: () => void }>();
  let next = 1;
  const scheduler: SyncFrameScheduler = {
    set(fn, ms) {
      const id = next++;
      timers.set(id, { at: now + ms, fn });
      return id;
    },
    clear(handle) {
      timers.delete(handle as number);
    },
  };
  return {
    scheduler,
    pending: () => timers.size,
    advance(ms: number) {
      now += ms;
      for (const [id, timer] of [...timers]) {
        if (timer.at <= now) {
          timers.delete(id);
          timer.fn();
        }
      }
    },
  };
}

/** A terminal that records each write and lets the test say when it has been parsed. */
function fakeTerm(autoComplete = true) {
  const writes: { text: string; bytes: Uint8Array; written: () => void }[] = [];
  return {
    writes,
    write(bytes: Uint8Array, written: () => void) {
      writes.push({ text: dec(bytes), bytes, written });
      if (autoComplete) written();
    },
    texts: () => writes.map((w) => w.text),
    all: () => writes.map((w) => w.text).join(""),
  };
}

function setup(over: { deadlineMs?: number; maxHeldBytes?: number } = {}) {
  const clock = fakeClock();
  const term = fakeTerm();
  const errors: unknown[] = [];
  const writer = createSyncFrameWriter({
    write: term.write,
    onError: (error) => errors.push(error),
    scheduler: clock.scheduler,
    ...over,
  });
  return { clock, term, writer, errors };
}

const noop = () => {};

describe("sync frame writer: where a write may end", () => {
  it("writes a chunk with no sync block straight through, once", () => {
    const { term, writer } = setup();
    writer.push(enc("hello\r\n\x1b[31mred\x1b[0m"), noop);
    expect(term.texts()).toEqual(["hello\r\n\x1b[31mred\x1b[0m"]);
    expect(writer.held()).toBe(0);
  });

  it("writes a chunk holding whole frames at once", () => {
    const { term, writer } = setup();
    const frames = `${ESC}[?2026h${ESC}[Hone${ESC}[?2026l${ESC}[?2026h${ESC}[Htwo${ESC}[?2026l`;
    writer.push(enc(frames), noop);
    expect(term.texts()).toEqual([frames]);
  });

  it("holds a chunk that ends inside an open block until the block closes", () => {
    const { term, writer } = setup();
    writer.push(enc(`before${ESC}[?2026h${ESC}[Hhalf`), noop);
    expect(term.texts()).toEqual(["before"]);
    expect(writer.held()).toBe(enc(`${ESC}[?2026h${ESC}[Hhalf`).length);
    writer.push(enc(` frame${ESC}[?2026l after`), noop);
    expect(term.texts()).toEqual(["before", `${ESC}[?2026h${ESC}[Hhalf frame${ESC}[?2026l after`]);
    expect(writer.held()).toBe(0);
  });

  it("a held block spanning many chunks is written as one write", () => {
    const { term, writer } = setup();
    writer.push(enc(`${ESC}[?2026h`), noop);
    for (let i = 0; i < 5; i++) writer.push(enc(`row ${i}\r\n`), noop);
    expect(term.writes).toHaveLength(0);
    writer.push(enc(`${ESC}[?2026l`), noop);
    expect(term.texts()).toEqual([`${ESC}[?2026hrow 0\r\nrow 1\r\nrow 2\r\nrow 3\r\nrow 4\r\n${ESC}[?2026l`]);
  });

  it("writes the closed frame and holds the next one opened in the same chunk", () => {
    const { term, writer } = setup();
    writer.push(enc(`${ESC}[?2026hA${ESC}[?2026l${ESC}[?2026hB`), noop);
    expect(term.texts()).toEqual([`${ESC}[?2026hA${ESC}[?2026l`]);
    writer.push(enc(`${ESC}[?2026l`), noop);
    expect(term.texts()).toEqual([`${ESC}[?2026hA${ESC}[?2026l`, `${ESC}[?2026hB${ESC}[?2026l`]);
  });

  it("an opening sequence split after its ESC is not written half-recognised past the block start", () => {
    const { term, writer } = setup();
    writer.push(enc(`text${ESC}`), noop);
    writer.push(enc(`[?2026hbody`), noop);
    // The ESC may already be written (xterm sits in ESCAPE state, sync off);
    // what must not be written is the rest of the block.
    expect(term.all()).not.toContain("body");
    writer.push(enc(`${ESC}[?2026l`), noop);
    expect(term.all()).toBe(`text${ESC}[?2026hbody${ESC}[?2026l`);
  });

  it("a closing sequence split across chunks keeps the block held until its final byte", () => {
    const { term, writer } = setup();
    writer.push(enc(`${ESC}[?2026hbody${ESC}[?20`), noop);
    expect(term.writes).toHaveLength(0);
    writer.push(enc("26"), noop);
    expect(term.writes).toHaveLength(0);
    writer.push(enc("lnext"), noop);
    expect(term.all()).toBe(`${ESC}[?2026hbody${ESC}[?2026lnext`);
  });

  it("an opening sequence split inside its digits is still an opening sequence", () => {
    const { term, writer } = setup();
    writer.push(enc(`${ESC}[?20`), noop);
    writer.push(enc("26hbody"), noop);
    expect(term.all()).toBe(`${ESC}[?20`);
    expect(writer.held()).toBe(enc("26hbody").length);
  });

  describe("sequence shapes xterm accepts", () => {
    const opens: [string, string][] = [
      ["plain", `${ESC}[?2026h`],
      ["with other modes first", `${ESC}[?1049;2026h`],
      ["with other modes after", `${ESC}[?2026;1049h`],
      ["with leading zeros", `${ESC}[?02026h`],
      ["with a sub-parameter on it", `${ESC}[?2026:1h`],
      ["with an empty parameter first", `${ESC}[?;2026h`],
      ["8-bit CSI", `${C1_CSI}?2026h`],
    ];
    for (const [name, open] of opens) {
      it(`${name} opens a block`, () => {
        const { term, writer } = setup();
        writer.push(enc(`x${open}body`), noop);
        expect(term.all()).toBe("x");
        writer.push(enc(`${ESC}[?2026l`), noop);
        expect(term.all()).toBe(`x${open}body${ESC}[?2026l`);
      });
    }

    const closes: [string, string][] = [
      ["plain", `${ESC}[?2026l`],
      ["with other modes", `${ESC}[?25;2026l`],
      ["8-bit CSI", `${C1_CSI}?2026l`],
    ];
    for (const [name, close] of closes) {
      it(`${name} closes a block`, () => {
        const { term, writer } = setup();
        writer.push(enc(`${ESC}[?2026hbody${close}tail`), noop);
        expect(term.all()).toBe(`${ESC}[?2026hbody${close}tail`);
        expect(writer.held()).toBe(0);
      });
    }

    const notOpens: [string, string][] = [
      ["a different private mode", `${ESC}[?25h`],
      ["2026 as a sub-parameter", `${ESC}[?1:2026h`],
      ["2026 in the non-private set mode", `${ESC}[2026h`],
      ["a mode query", `${ESC}[?2026$p`],
      ["2026 with an intermediate", `${ESC}[?2026 h`],
      ["a larger number", `${ESC}[?20260h`],
      ["a reset", `${ESC}[?2026l`],
      ["a CSI aborted by CAN", `${ESC}[?2026\x18h`],
      ["a CSI aborted by SUB", `${ESC}[?20\x1a26h`],
      ["8-bit CSI cut by ESC", `${C1_CSI}?2026${ESC}h`],
    ];
    for (const [name, text] of notOpens) {
      it(`${name} does not open a block`, () => {
        const { term, writer } = setup();
        writer.push(enc(`${text}after`), noop);
        expect(term.all()).toBe(`${text}after`);
        expect(writer.held()).toBe(0);
      });
    }

    it("a C0 control inside the CSI does not cancel it", () => {
      const { term, writer } = setup();
      writer.push(enc(`${ESC}[?20\r26hbody`), noop);
      expect(term.all()).toBe("");
      expect(writer.held()).toBeGreaterThan(0);
    });

    it("ignores parameters past xterm's 32", () => {
      const { term, writer } = setup();
      const many = Array.from({ length: 32 }, () => "1").join(";");
      writer.push(enc(`${ESC}[?${many};2026hbody`), noop);
      expect(term.all()).toBe(`${ESC}[?${many};2026hbody`);
    });

    it("a lone C2 that is not a C1 pair is text, and a C2 split from its C1 byte still opens", () => {
      const { term, writer } = setup();
      writer.push(new Uint8Array([0xc2, 0xb0]), noop); // degree sign
      expect(term.writes).toHaveLength(1);
      writer.push(new Uint8Array([0xc2]), noop);
      writer.push(Uint8Array.from([0x9b, ...enc("?2026hbody")]), noop);
      expect(term.writes.slice(1).map((w) => w.bytes.length)).toEqual([1]);
      expect(writer.held()).toBe(enc("?2026hbody").length + 1);
    });
  });

  describe("string payloads never open or close a block", () => {
    const strings: [string, string][] = [
      ["OSC ended by BEL", `${ESC}]0;[?2026h\x07`],
      ["OSC ended by ST", `${ESC}]0;[?2026h${ESC}\\`],
      ["OSC ended by 8-bit ST", `${ESC}]0;[?2026h\u009c`],
      ["DCS", `${ESC}Pq[?2026h${ESC}\\`],
      ["APC", `${ESC}_Gi=1;[?2026h${ESC}\\`],
      ["PM", `${ESC}^[?2026h${ESC}\\`],
      ["SOS", `${ESC}X[?2026h${ESC}\\`],
    ];
    for (const [name, text] of strings) {
      it(`${name}: look-alike bytes inside do not open a block`, () => {
        const { term, writer } = setup();
        writer.push(enc(`${text}tail`), noop);
        expect(term.all()).toBe(`${text}tail`);
        expect(writer.held()).toBe(0);
      });
      it(`${name}: look-alike bytes inside do not close a real block`, () => {
        const { term, writer } = setup();
        writer.push(enc(`${ESC}[?2026h${text.replace("2026h", "2026l")}body`), noop);
        expect(term.writes).toHaveLength(0);
      });
    }

    it("an unterminated string still lets the next ESC sequence open a block", () => {
      const { term, writer } = setup();
      writer.push(enc(`${ESC}]0;unterminated${ESC}[?2026hbody`), noop);
      expect(term.all()).toBe(`${ESC}]0;unterminated`);
    });

    it("BEL inside a DCS payload does not end it", () => {
      const { term, writer } = setup();
      // If BEL ended the DCS the following ESC [ ? 2026 h would still open a
      // block, so make the look-alike have no ESC: only the DCS state hides it.
      writer.push(enc(`${ESC}Pq\x07[?2026h${ESC}\\tail`), noop);
      expect(term.all()).toBe(`${ESC}Pq\x07[?2026h${ESC}\\tail`);
    });
  });
});

describe("sync frame writer: bounds, which fail open", () => {
  it("writes a block that never closes when the deadline passes, and bypasses it afterwards", () => {
    const { term, writer, clock } = setup();
    writer.push(enc(`${ESC}[?2026hnever closes`), noop);
    expect(term.writes).toHaveLength(0);
    clock.advance(SYNC_HOLD_DEADLINE_MS - 1);
    expect(term.writes).toHaveLength(0);
    clock.advance(1);
    expect(term.texts()).toEqual([`${ESC}[?2026hnever closes`]);
    // Still inside the block as far as xterm is concerned: pass straight through
    // rather than stalling every later chunk for another deadline.
    writer.push(enc("more"), noop);
    expect(term.texts()).toEqual([`${ESC}[?2026hnever closes`, "more"]);
    expect(clock.pending()).toBe(0);
  });

  it("after a forced release, a close restores holding for the next block", () => {
    const { term, writer, clock } = setup();
    writer.push(enc(`${ESC}[?2026hx`), noop);
    clock.advance(SYNC_HOLD_DEADLINE_MS);
    writer.push(enc(`y${ESC}[?2026l`), noop);
    writer.push(enc(`${ESC}[?2026hz`), noop);
    expect(term.all()).toBe(`${ESC}[?2026hxy${ESC}[?2026l`);
    expect(writer.held()).toBe(enc(`${ESC}[?2026hz`).length);
  });

  it("the deadline runs from when holding began, not from the latest chunk", () => {
    const { term, writer, clock } = setup();
    writer.push(enc(`${ESC}[?2026h`), noop);
    clock.advance(60);
    writer.push(enc("a"), noop);
    clock.advance(60);
    expect(term.texts()).toEqual([`${ESC}[?2026ha`]);
  });

  it("a new block opened after one closed gets a fresh deadline", () => {
    const { term, writer, clock } = setup();
    writer.push(enc(`${ESC}[?2026h`), noop);
    clock.advance(90);
    writer.push(enc(`${ESC}[?2026l${ESC}[?2026h`), noop);
    clock.advance(90);
    expect(term.texts()).toEqual([`${ESC}[?2026h${ESC}[?2026l`]);
    clock.advance(10);
    expect(term.texts()).toHaveLength(2);
  });

  it("ends the deadline timer when the block closes", () => {
    const { writer, clock } = setup();
    writer.push(enc(`${ESC}[?2026hx`), noop);
    expect(clock.pending()).toBe(1);
    writer.push(enc(`${ESC}[?2026l`), noop);
    expect(clock.pending()).toBe(0);
  });

  it("releases the block when held bytes pass the cap, never holding more than the cap", () => {
    const { term, writer } = setup();
    const chunk = new Uint8Array(40 * 1024).fill(0x61);
    writer.push(enc(`${ESC}[?2026h`), noop);
    let writesBefore = 0;
    for (let i = 0; i < 20; i++) {
      writer.push(chunk, noop);
      expect(writer.held()).toBeLessThanOrEqual(SYNC_HOLD_MAX_BYTES);
      if (writesBefore === 0 && term.writes.length) writesBefore = i + 1;
    }
    expect(writesBefore).toBe(4); // 4 * 40 KiB passes 128 KiB
    expect(term.writes.length).toBeGreaterThan(1); // then bypass: each chunk goes straight through
  });

  it("a single chunk bigger than the cap inside a block is written at once", () => {
    const { term, writer } = setup();
    const big = new Uint8Array(1024 * 1024).fill(0x62);
    writer.push(enc(`${ESC}[?2026h`), noop);
    writer.push(big, noop);
    expect(writer.held()).toBe(0);
    expect(term.writes.reduce((n, w) => n + w.bytes.length, 0)).toBe(big.length + 8);
  });

  it("a megabyte inside one block in 64 KiB messages never holds past the cap", () => {
    const { term, writer } = setup();
    writer.push(enc(`${ESC}[?2026h`), noop);
    let peak = 0;
    for (let i = 0; i < 16; i++) {
      writer.push(new Uint8Array(64 * 1024).fill(0x63), noop);
      peak = Math.max(peak, writer.held());
    }
    expect(peak).toBeLessThanOrEqual(SYNC_HOLD_MAX_BYTES);
    writer.push(enc(`${ESC}[?2026l`), noop);
    expect(term.writes.reduce((n, w) => n + w.bytes.length, 0)).toBe(8 + 16 * 64 * 1024 + 8);
  });

  it("repeated opens without a close are one block", () => {
    const { term, writer } = setup();
    writer.push(enc(`a${ESC}[?2026hb${ESC}[?2026hc`), noop);
    expect(term.texts()).toEqual(["a"]);
    writer.push(enc(`${ESC}[?2026l`), noop);
    expect(term.all()).toBe(`a${ESC}[?2026hb${ESC}[?2026hc${ESC}[?2026l`);
  });

  it("a close without an open changes nothing", () => {
    const { term, writer } = setup();
    writer.push(enc(`a${ESC}[?2026lb`), noop);
    expect(term.texts()).toEqual([`a${ESC}[?2026lb`]);
  });

  it("the cap is a quarter of the credit window, so held bytes cannot starve the reader", () => {
    expect(SYNC_HOLD_MAX_BYTES).toBe(OUTPUT_CREDIT_WINDOW / 4);
    // Held + a full coalesced-ack debt + one message must fit the window.
    expect(SYNC_HOLD_MAX_BYTES + SYNC_HOLD_MAX_BYTES + 64 * 1024).toBeLessThan(OUTPUT_CREDIT_WINDOW);
    expect(SYNC_HOLD_DEADLINE_MS).toBeLessThan(1000); // xterm's own timeout
  });

  it("flush writes held bytes and lets later chunks through until the block closes", () => {
    const { term, writer, clock } = setup();
    writer.push(enc(`${ESC}[?2026hx`), noop);
    writer.flush();
    expect(term.texts()).toEqual([`${ESC}[?2026hx`]);
    expect(clock.pending()).toBe(0);
    writer.push(enc("y"), noop);
    expect(term.texts()).toEqual([`${ESC}[?2026hx`, "y"]);
  });

  it("reset writes held bytes and forgets the open block", () => {
    const { term, writer } = setup();
    writer.push(enc(`${ESC}[?2026hx`), noop);
    writer.reset();
    writer.push(enc(`y${ESC}[?2026hz`), noop);
    expect(term.texts()).toEqual([`${ESC}[?2026hx`, "y"]);
  });

  it("reset forgets a half-seen sequence", () => {
    const { term, writer } = setup();
    writer.push(enc(`${ESC}[?20`), noop);
    writer.reset();
    writer.push(enc("26hbody"), noop);
    expect(term.all()).toBe(`${ESC}[?2026hbody`);
  });

  it("a flush with nothing held writes nothing", () => {
    const { term, writer } = setup();
    writer.flush();
    writer.flush();
    expect(term.writes).toHaveLength(0);
  });
});

describe("sync frame writer: credit", () => {
  /** The session's real credit objects, wired the way TerminalSession wires them. */
  function credited(autoComplete = true) {
    const clock = fakeClock();
    const term = fakeTerm(autoComplete);
    const sent: number[] = [];
    const acks = createAckCoalescer((_, bytes) => sent.push(bytes), (flush) => {
      void flush;
      return () => {};
    });
    const credit = createOutputCredit();
    const errors: unknown[] = [];
    const writer = createSyncFrameWriter({
      write: term.write,
      onError: (e) => errors.push(e),
      scheduler: clock.scheduler,
    });
    let received = 0;
    const view = { terminal: true, disposed: false };
    function receive(text: string | Uint8Array) {
      const bytes = typeof text === "string" ? enc(text) : text;
      received += bytes.length;
      const decision = credit.accept(bytes, "s", view);
      if (decision.action !== "paint") throw new Error(decision.action);
      const token = decision.token;
      writer.push(token.bytes, () => acks.add("s", token.release()));
    }
    const acked = () => {
      acks.flush();
      return sent.reduce((a, b) => a + b, 0);
    };
    return { writer, term, clock, credit, receive, received: () => received, acked, errors };
  }

  it("acknowledges a held chunk only when it is written", () => {
    const t = credited();
    t.receive(`${ESC}[?2026hbody`);
    expect(t.acked()).toBe(0);
    expect(t.credit.pending()).toBe(enc(`${ESC}[?2026hbody`).length);
    t.receive(`${ESC}[?2026l`);
    expect(t.acked()).toBe(t.received());
    expect(t.credit.pending()).toBe(0);
  });

  it("a chunk split into a written part and a held part is acknowledged once, after both are written", () => {
    const t = credited(false);
    t.receive(`text${ESC}[?2026hbody`);
    expect(t.term.writes).toHaveLength(1);
    t.term.writes[0].written();
    expect(t.acked()).toBe(0); // the held tail is still owed
    t.receive(`${ESC}[?2026l`);
    t.term.writes[1].written();
    expect(t.acked()).toBe(t.received());
  });

  it("a deadline release acknowledges what it wrote", () => {
    const t = credited();
    t.receive(`${ESC}[?2026hx`);
    t.clock.advance(SYNC_HOLD_DEADLINE_MS);
    expect(t.acked()).toBe(t.received());
  });

  it("holding at the cap leaves the reader at least three quarters of the window", () => {
    const t = credited();
    t.receive(`${ESC}[?2026h`);
    for (let i = 0; i < 12; i++) {
      t.receive(new Uint8Array(32 * 1024).fill(0x64));
      expect(t.credit.pending()).toBeLessThanOrEqual(SYNC_HOLD_MAX_BYTES + 32 * 1024);
      expect(t.credit.pending()).toBeLessThan(OUTPUT_CREDIT_WINDOW - 64 * 1024);
    }
  });

  it("discard acknowledges what was held and writes nothing", () => {
    const t = credited();
    t.receive(`a${ESC}[?2026hbody`);
    const written = t.term.writes.length;
    t.writer.discard();
    expect(t.term.writes).toHaveLength(written);
    expect(t.acked()).toBe(t.received());
    expect(t.clock.pending()).toBe(0);
  });

  it("after discard a push is acknowledged and not written", () => {
    const t = credited();
    t.writer.discard();
    t.receive("late");
    expect(t.term.writes).toHaveLength(0);
    expect(t.acked()).toBe(4);
  });

  it("a write that throws still acknowledges its chunk and reports the error", () => {
    const clock = fakeClock();
    let calls = 0;
    const done = vi.fn();
    const writer = createSyncFrameWriter({
      write: () => {
        calls++;
        throw new Error("disposed");
      },
      scheduler: clock.scheduler,
    });
    expect(() => writer.push(enc("x"), done)).toThrow("disposed");
    expect(calls).toBe(1);
    expect(done).toHaveBeenCalledTimes(1);
  });

  it("a deadline flush that throws is reported through onError and acknowledges the held chunk", () => {
    const clock = fakeClock();
    const errors: unknown[] = [];
    const done = vi.fn();
    const writer = createSyncFrameWriter({
      write: () => {
        throw new Error("disposed");
      },
      onError: (e) => errors.push(e),
      scheduler: clock.scheduler,
    });
    writer.push(enc(`${ESC}[?2026hx`), done);
    expect(done).not.toHaveBeenCalled();
    clock.advance(SYNC_HOLD_DEADLINE_MS);
    expect(errors).toHaveLength(1);
    expect(done).toHaveBeenCalledTimes(1);
  });

  it("an empty chunk is acknowledged without a write", () => {
    const { term, writer } = setup();
    const done = vi.fn();
    writer.push(new Uint8Array(0), done);
    expect(done).toHaveBeenCalledTimes(1);
    expect(term.writes).toHaveLength(0);
  });
});

describe("sync frame writer: session events", () => {
  function writerFor(): { writer: SyncFrameWriter; term: ReturnType<typeof fakeTerm> } {
    const { writer, term } = setup();
    return { writer, term };
  }

  it("the session closing while bytes are held loses nothing: flush writes them in order", () => {
    const { writer, term } = writerFor();
    writer.push(enc(`a${ESC}[?2026hb`), noop);
    writer.flush();
    term.write(enc("[session closed]"), noop);
    expect(term.all()).toBe(`a${ESC}[?2026hb[session closed]`);
  });

  it("a restart (reset) while a block is held starts the next process clean", () => {
    const { writer, term } = writerFor();
    writer.push(enc(`${ESC}[?2026hold process`), noop);
    writer.reset();
    writer.push(enc(`new process${ESC}[?2026hframe`), noop);
    expect(term.texts()).toEqual([`${ESC}[?2026hold process`, "new process"]);
  });

  it("an adoption replay that starts in the middle of a block holds nothing it cannot finish", () => {
    const { writer, term, } = setup();
    // The ring's tail begins inside a frame: bytes of a block whose opener was
    // trimmed away. No open was seen, so nothing is held.
    writer.push(enc(`tail of a frame${ESC}[?2026l`), noop);
    writer.push(enc(`${ESC}[?2026hnext`), noop);
    expect(term.texts()).toEqual([`tail of a frame${ESC}[?2026l`]);
  });
});
