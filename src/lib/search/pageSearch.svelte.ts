/**
 * The page search: one field in the view bar that searches whatever the
 * active view and section show.
 *
 * A pane that has something to search registers a provider for its place,
 * `"<view>:<section>"`. The view bar asks for the provider of the place on
 * screen, so a diff mounted behind another section can never take the field
 * or the chord. Each provider keeps its own query: a find string typed into a
 * diff never becomes History's commit filter on a section switch.
 *
 * Two kinds, because filtering and finding are different acts:
 * - `filter` narrows a list (History's commits): the query stays applied
 *   when the field loses focus, and Enter does nothing more.
 * - `find` steps through matches in what is shown (a diff, a file): Enter
 *   and Shift+Enter step, and it reports a count and whether the pattern is
 *   valid.
 *
 * A place with no provider is not dead: the field searches the repository
 * through the command palette there.
 *
 * The scope travels by Svelte context. The view that owns a pane says which
 * place it is (`providePageSearchScope`); a viewer inside it registers with
 * that scope and drops its own toolbar Find. The same viewer mounted
 * anywhere else (a stash preview, a dialog) has no scope and keeps its own.
 */
import { getContext, setContext, untrack } from "svelte";
import type { ViewTab } from "../repos/persist";

export type PageScope = `${ViewTab}:${string}`;

export interface FindStatus {
  /** "3 of 12", "no matches", "bad pattern". */
  text: string;
  invalid: boolean;
  hasMatches: boolean;
}

interface ProviderBase {
  /** What is searched, for assistive tech and the tooltip: "Find in this diff". */
  label: string;
  placeholder: string;
  query(): string;
  setQuery(next: string): void;
}

export interface FilterProvider extends ProviderBase {
  kind: "filter";
  /** A second narrowing shown in the field (History's branch), with its own clear. */
  chip?(): { label: string; title: string; clear(): void } | null;
}

export interface FindProvider extends ProviderBase {
  kind: "find";
  caseSensitive(): boolean;
  setCaseSensitive(next: boolean): void;
  regex(): boolean;
  setRegex(next: boolean): void;
  status(): FindStatus;
  step(delta: 1 | -1): void;
}

export type PageSearchProvider = FilterProvider | FindProvider;

let providers = $state.raw<Readonly<Record<string, PageSearchProvider>>>({});

/**
 * Registers `provider` for `scope` until the returned disposer runs. A later
 * registration for the same scope replaces an earlier one, and the earlier
 * one's disposer then leaves it alone.
 *
 * Callers register from an `$effect`. The registry is read untracked here, or
 * the effect would depend on the state it writes and rerun without end
 * (`effect_update_depth_exceeded`).
 */
export function registerPageSearch(scope: PageScope, provider: PageSearchProvider): () => void {
  untrack(() => {
    providers = { ...providers, [scope]: provider };
  });
  return () =>
    untrack(() => {
      if (providers[scope] !== provider) return;
      const rest: Record<string, PageSearchProvider> = {};
      for (const [key, value] of Object.entries(providers)) if (key !== scope) rest[key] = value;
      providers = rest;
    });
}

/** The provider for the place on screen, or null when it has none. */
export function pageSearchFor(scope: PageScope | null): PageSearchProvider | null {
  return scope ? (providers[scope] ?? null) : null;
}

/** A narrowing that stays applied across a view's sections, shown as a chip with its own clear. */
export interface StandingFilter {
  label: string;
  title: string;
  clear(): void;
}

let standing = $state.raw<Readonly<Partial<Record<ViewTab, () => StandingFilter | null>>>>({});

/**
 * A view whose filter narrows more than the section that edits it (History's
 * commit filter also narrows the diff's commit list and the reflog) says so
 * here. Where the field is not that filter, it shows the chip, so an applied
 * filter never narrows a list out of sight.
 */
export function registerStandingFilter(view: ViewTab, read: () => StandingFilter | null): () => void {
  // Untracked for the reason registerPageSearch gives.
  untrack(() => {
    standing = { ...standing, [view]: read };
  });
  return () =>
    untrack(() => {
      if (standing[view] !== read) return;
      const rest: Partial<Record<ViewTab, () => StandingFilter | null>> = {};
      for (const [key, value] of Object.entries(standing) as [ViewTab, () => StandingFilter | null][]) {
        if (key !== view) rest[key] = value;
      }
      standing = rest;
    });
}

export function standingFilterFor(view: ViewTab): StandingFilter | null {
  return standing[view]?.() ?? null;
}

export function pageScope(view: ViewTab, section: string | null): PageScope {
  return `${view}:${section ?? ""}`;
}

const SCOPE = Symbol("page-search-scope");

/** Called by the view that owns a pane: everything inside it searches as `scope`. */
export function providePageSearchScope(scope: PageScope): void {
  setContext(SCOPE, scope);
}

/** The place a viewer was mounted as, or null when it is not a page's own pane. */
export function pageSearchScope(): PageScope | null {
  return getContext<PageScope | undefined>(SCOPE) ?? null;
}
