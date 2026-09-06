import { afterEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { interfaceStore } from "../stores/interfaceStore";
import { timestampFormat } from "./timestampFormat";
import { formatTimestamp, timestampTitle } from "./timestampStyle";

/** 2026-09-06 12:00 local, and a "now" three days later. */
const SAMPLE = Math.floor(new Date(2026, 8, 6, 12, 0, 0).getTime() / 1000);
const LATER = SAMPLE + 3 * 86_400;

afterEach(() => interfaceStore.reset());

describe("timestampFormat", () => {
  it("formats through the stored style", () => {
    expect(get(timestampFormat).text(SAMPLE, LATER)).toBe("3d ago");
    interfaceStore.setTimestampStyle("absolute");
    expect(get(timestampFormat).text(SAMPLE, LATER)).toBe("2026-09-06");
  });

  it("re-derives when the preference changes, so subscribers follow", () => {
    const seen: string[] = [];
    const stop = timestampFormat.subscribe((format) =>
      seen.push(format.text(SAMPLE, LATER)),
    );
    interfaceStore.setTimestampStyle("absolute");
    interfaceStore.setTimestampStyle("relative");
    stop();
    expect(seen).toEqual(["3d ago", "2026-09-06", "3d ago"]);
  });

  it("hands back the same strings as the pure formatters", () => {
    // The store is a subscription wrapper, not a second implementation: if
    // these ever diverge, one surface would honour the preference differently
    // from the next.
    for (const style of ["relative", "absolute"] as const) {
      interfaceStore.setTimestampStyle(style);
      const format = get(timestampFormat);
      expect(format.text(SAMPLE, LATER)).toBe(formatTimestamp(SAMPLE, style, LATER));
      expect(format.title(SAMPLE, LATER)).toBe(timestampTitle(SAMPLE, style, LATER));
    }
  });
});
