import { derived, type Readable } from "svelte/store";
import { interfaceStore } from "../stores/interfaceStore";
import { formatTimestamp, timestampTitle } from "./timestampStyle";

export interface TimestampFormatter {
  /** The commit time in the reader's chosen style; "" for a falsy timestamp. */
  readonly text: (timestampSec: number, nowSec?: number) => string;
  /** The other style, for a `title` — never the same string as `text`. */
  readonly title: (timestampSec: number, nowSec?: number) => string;
}

/**
 * The timestamp preference as a pair of ready-to-call formatters.
 *
 * Components subscribe to this instead of reading the preference and
 * branching, which is what keeps the setting from being honoured in the
 * commit list and quietly ignored in the tooltip beside it: there is one
 * subscription and one pair of functions, and adding a fourth timestamp
 * surface costs `$timestampFormat.text(ts)` rather than another branch.
 */
export const timestampFormat: Readable<TimestampFormatter> = derived(
  interfaceStore,
  ($prefs) => ({
    text: (timestampSec: number, nowSec?: number) =>
      formatTimestamp(timestampSec, $prefs.timestampStyle, nowSec),
    title: (timestampSec: number, nowSec?: number) =>
      timestampTitle(timestampSec, $prefs.timestampStyle, nowSec),
  }),
);
