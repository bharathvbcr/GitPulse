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
        stopped = true;
        onError(`Input delivery failed; restart before typing again. ${String(error)}`, true);
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
