/**
 * The seam between a cached fleet language row and the language bar's own
 * vocabulary.
 *
 * `FleetLanguageStat` mirrors `RepoLanguageStat` field for field so the rows
 * the ledger cached fold through `pickLanguageBarStats` — the one function
 * that decides what a language mix looks like anywhere in this app. The
 * alternative was a second fold that agreed with the first until the day it
 * did not, and then quietly disagreed about which languages a repository is
 * written in depending on which screen you were looking at.
 *
 * Nothing here converts anything. The two shapes are held together at compile
 * time and the rows are passed straight through, which is the point: a
 * conversion function is a place for drift to live.
 */

import type { RepoLanguageStat } from "../language/barStats";
import { pickLanguageBarStats, type LanguageStat } from "../language/barStats";
import type { FleetLanguageStat } from "./types";

/**
 * Compile-time proof that the two shapes are the same shape.
 *
 * Assigning in both directions is what makes this a contract rather than a
 * one-way cast: a field added to either side, or a type changed on either
 * side, fails `npm run check` here instead of surfacing as a language bar that
 * silently renders nothing on the Fleet page. `check:types` separately pins
 * the Rust half against `FleetLanguageStat`, so the chain runs from the SQLite
 * row all the way to the bar.
 */
type Extends<A, B> = A extends B ? true : false;
type AssertTrue<T extends true> = T;
export type LanguageStatContract = [
  AssertTrue<Extends<FleetLanguageStat, RepoLanguageStat>>,
  AssertTrue<Extends<RepoLanguageStat, FleetLanguageStat>>,
];

/**
 * Folds cached rows into the bar's segments, largest first with a remainder.
 *
 * Returns an empty list for an empty input rather than a placeholder segment:
 * a repository with no breakdown on file must render as *absent*, never as a
 * bar of one grey band that reads like a measurement of something.
 */
export function foldFleetLanguages(
  languages: readonly FleetLanguageStat[],
  maxShown?: number,
): LanguageStat[] {
  if (languages.length === 0) return [];
  return pickLanguageBarStats([...languages], maxShown);
}

/**
 * Sums one language across repositories, so the fleet mix is a fleet mix.
 *
 * Percentages are deliberately NOT averaged. A 100%-Rust repository of 200
 * lines and a 100%-TypeScript repository of 200,000 do not make a fleet that
 * is half Rust, and averaging the two percentages is exactly how a dashboard
 * comes to say that. Lines are the only thing that adds up, so the share is
 * recomputed from the summed lines at the end.
 *
 * `counted` is the number of repositories that contributed at least one
 * language row. It travels with the result because a mix drawn from four of
 * twenty-one repositories is not a picture of the fleet, and the caller has to
 * be able to say so.
 */
export function mergeLanguageStats(
  perRepo: readonly (readonly FleetLanguageStat[])[],
): { stats: FleetLanguageStat[]; totalLines: number; counted: number } {
  const merged = new Map<string, FleetLanguageStat>();
  let totalLines = 0;
  let counted = 0;
  for (const languages of perRepo) {
    if (languages.length === 0) continue;
    counted += 1;
    for (const stat of languages) {
      // A non-finite line count would poison the whole total into NaN, which
      // renders as a broken bar rather than as the one bad row it is.
      const lines = Number.isFinite(stat.code_lines) ? stat.code_lines : 0;
      const files = Number.isFinite(stat.file_count) ? stat.file_count : 0;
      totalLines += lines;
      const prev = merged.get(stat.language);
      if (!prev) {
        merged.set(stat.language, { ...stat, code_lines: lines, file_count: files, percentage: 0 });
        continue;
      }
      prev.code_lines += lines;
      prev.file_count += files;
    }
  }
  const stats = [...merged.values()];
  for (const stat of stats) {
    stat.percentage = totalLines > 0 ? (stat.code_lines / totalLines) * 100 : 0;
  }
  stats.sort((a, b) => b.code_lines - a.code_lines || a.language.localeCompare(b.language));
  return { stats, totalLines, counted };
}
