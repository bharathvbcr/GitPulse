/** Ordered user input. Never retry a write: it may have been partially accepted. */
const MAX_INPUT_BYTES = 1024 * 1024;
const CHUNK_BYTES = 16 * 1024;
const encoder = new TextEncoder();

export function terminalDeadline<T>(work: Promise<T>, ms: number, label: string): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`${label} timed out`)), ms);
    void work.then(resolve, reject).finally(() => clearTimeout(timer));
  });
}

/**
 * Whether a failed send ends the tab's input for good. Two things do: the
 * session is gone (nothing left to type into), or the send never answered, so
 * it may still be in flight and a later write could land out of order. A
 * rejected write is different: the backend answered, the outcome is known, and
 * ending the tab over it (a restart kills the process, an agent included)
 * destroys more than the write.
 */
export function inputIsDead(error: unknown): boolean {
  if (error instanceof Error && error.message === "Terminal input timed out") return true;
  return /\bnot found\b|\bsession is closed\b|\bTerminal closed\b/i.test(String(error));
}

export function createTerminalInput(
  send: (data: string, binary: boolean) => Promise<void>,
  onError: (message: string, fatal: boolean) => void,
) {
  const queue: Array<{ data: string; binary: boolean }> = [];
  let bytes = 0;
  let stopped = false;
  let pumping: Promise<void> | null = null;

  async function pump() {
    try {
      while (!stopped && queue.length) {
        const chunk = queue.shift()!;
        await terminalDeadline(send(chunk.data, chunk.binary), 5000, "Terminal input");
        bytes -= chunk.binary ? chunk.data.length : encoder.encode(chunk.data).length;
      }
    } catch (error) {
      if (!stopped) {
        if (inputIsDead(error)) {
          stopped = true;
          onError(`Input delivery failed; restart before typing again. ${String(error)}`, true);
        } else {
          // The failed chunk may have been partly accepted, so it is never
          // retried, and what was queued behind it was typed after the gap and
          // would land in the wrong place. Both are dropped and said so; the
          // next thing the user types goes through.
          queue.length = 0;
          bytes = 0;
          onError(`Some input was not delivered and was dropped. ${String(error)}`, false);
        }
      }
    } finally {
      if (stopped) { queue.length = 0; bytes = 0; }
      pumping = null;
    }
  }

  return {
    write(data: string, binary = false): boolean {
      if (stopped) return false;
      if (!data) return true;
      const length = binary ? data.length : data.length > MAX_INPUT_BYTES ? data.length : encoder.encode(data).length;
      if (data.length > MAX_INPUT_BYTES || bytes + length > MAX_INPUT_BYTES) {
        onError("Paste was not sent: pending terminal input is limited to 1 MiB.", false);
        return false;
      }
      // Rust strings require Unicode scalar values. With the u flag this
      // matches isolated surrogates only, leaving valid pairs untouched.
      if (!binary && /[\uD800-\uDFFF]/u.test(data)) {
        onError("Invalid Unicode terminal input was not sent.", false);
        return false;
      }
      if (binary && Array.from(data).some((c) => (c.codePointAt(0) ?? 0) > 255)) {
        onError("Invalid binary terminal input was not sent.", false);
        return false;
      }
      const previous = queue.at(-1);
      let chunk = previous?.binary === binary ? queue.pop()!.data : "";
      let size = binary ? chunk.length : encoder.encode(chunk).length;
      // Iteration is by Unicode code point, so a chunk boundary never bisects
      // a surrogate pair. Bracketed-paste markers remain in the same stream.
      for (const character of data) {
        const code = character.codePointAt(0) ?? 0;
        const width = binary || code <= 0x7f ? 1 : code <= 0x7ff ? 2 : code <= 0xffff ? 3 : 4;
        if (size + width > CHUNK_BYTES) { queue.push({ data: chunk, binary }); chunk = ""; size = 0; }
        chunk += character;
        size += width;
      }
      if (chunk) queue.push({ data: chunk, binary });
      bytes += length;
      if (!pumping) pumping = pump();
      return true;
    },
    idle: () => pumping ?? Promise.resolve(),
    dispose() { stopped = true; queue.length = 0; },
  };
}
