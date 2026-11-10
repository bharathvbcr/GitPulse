/**
 * The copy vocabulary: the glossary, the verb map and the banned-term list as
 * data (docs/design/UI_RULES.md §3, DESIGN_LANGUAGE.md §10).
 *
 * Plain constants and one pure helper, no imports, so the design lint
 * (`scripts/design-lint.mjs` rule 14), the copy helpers and tests can all read
 * the same list. `BANNED_UI_TERMS` is a contract with that lint: its name and
 * the `BannedTerm` shape do not change without changing both sides.
 *
 * What the list can and cannot do: every pattern is a *text* approximation.
 * It sees characters, not meaning, so it cannot tell "harness" the noun from
 * "harness" in a path, nor `devmap` in a code-formatted CLI command from
 * `devmap` in a sentence. Each entry's `note` says where it over- or
 * under-matches. A hit is a candidate for review, never proof of a violation.
 */

/** One entry of the "Say / Never say" glossary. */
export interface GlossaryEntry {
  /** What the user is looking at, in the user's words. */
  readonly concept: string;
  /** The word the interface uses. */
  readonly say: string;
  /** Synonyms the interface must not use for this concept. */
  readonly never: readonly string[];
  readonly note?: string;
}

export const GLOSSARY: readonly GlossaryEntry[] = [
  {
    concept: "The indexed code graph",
    say: "code map",
    never: ["code graph", "repo map", "devmap", "codeintel"],
    note: 'The Code → Map section keeps the label "Map". "DevMap" names the CLI only in Settings → Agents.',
  },
  {
    concept: "The policy tool",
    say: "Manvi",
    never: ["MANVI"],
    note: 'Written "Manvi policy" in sentences.',
  },
  {
    concept: "GitPulse's own gate",
    say: "checks",
    never: ["harness", "sidecar", "posture"],
    note: 'Written "GitPulse\'s checks". The technical term stays in code.',
  },
  {
    concept: "One execution of a task",
    say: "run",
    never: ["attempt"],
    note: 'A failed run that was repeated is "run 2".',
  },
  {
    concept: "A visible terminal or agent process",
    say: "session",
    never: [],
  },
  {
    concept: "Linked checkout",
    say: "worktree",
    never: ["lane", "linked checkout"],
    note: 'The verb "checkout" (git checkout a branch) is not a synonym of worktree and is not banned.',
  },
  {
    concept: "Board item",
    say: "task",
    never: ["ticket"],
    note: '"Card" only for the physical card on the board.',
  },
  {
    concept: "Repository",
    say: "repository",
    never: ["repo", "project"],
    note: "Fine in code and identifiers.",
  },
  {
    concept: "Ref",
    say: "branch",
    never: [],
    note: '"ref" only in Refs and Reflog, or when it genuinely is a tag or remote ref.',
  },
  {
    concept: "Commit",
    say: "commit",
    never: [],
    note: '"changes" only for uncommitted edits.',
  },
  {
    concept: "App settings",
    say: "Settings",
    never: ["Preferences", "Options"],
  },
];

/** One action verb with its single meaning. */
export interface VerbEntry {
  readonly verb: string;
  /** The one meaning this verb carries, in a sentence. */
  readonly meaning: string;
}

export const VERBS: readonly VerbEntry[] = [
  { verb: "Refresh", meaning: "Re-read local state." },
  { verb: "Fetch", meaning: "Contact the remote." },
  { verb: "Rescan", meaning: "Run a scanner again (coverage, secrets, health)." },
  { verb: "Rebuild", meaning: "Regenerate an index." },
  { verb: "Try again", meaning: "Repeat an action that just failed. Only ever offered after a failure." },
  { verb: "Delete", meaning: "Destroy permanently (a branch, a worktree, a file)." },
  { verb: "Discard", meaning: "Throw away uncommitted changes." },
  { verb: "Remove", meaning: "Take something out of a list without destroying it." },
  { verb: "Archive", meaning: "A task's Done state." },
  { verb: "Clear", meaning: "Empty a field or a filter." },
  { verb: "Dismiss", meaning: "Close a notice or toast without acting on it." },
  { verb: "Open", meaning: "Navigate to something." },
  { verb: "Show", meaning: "Make something visible. Pairs with Hide." },
  { verb: "Hide", meaning: "Make something invisible. Pairs with Show." },
  { verb: "Run", meaning: "Start a process." },
  { verb: "Stop", meaning: "End a process." },
];

/** A verb that no longer appears in the interface, and what replaces it. */
export interface RetiredVerb {
  readonly verb: string;
  readonly use: string;
}

export const RETIRED_VERBS: readonly RetiredVerb[] = [
  { verb: "Reload", use: "Refresh" },
  { verb: "Recheck", use: "Rescan" },
  { verb: "Retry", use: "Try again" },
  { verb: "Launch", use: "Run (a process) or Open (a place)" },
  { verb: "Organise", use: "Organize" },
];

/**
 * A term that belongs to another app and is deliberately NOT banned here.
 * Renaming it in GitPulse alone would be cross-app copy drift, so it changes
 * in both apps or neither.
 */
export interface CrossAppTerm {
  readonly term: string;
  readonly owner: string;
  readonly note: string;
}

export const CROSS_APP_TERMS: readonly CrossAppTerm[] = [
  {
    term: "verdict",
    owner: "Manvi",
    note: "Manvi's own word, and it appears in Manvi's CLI output. Check Manvi's vocabulary and change both apps or neither. Not in BANNED_UI_TERMS.",
  },
];

/**
 * One banned term. `pattern` has no `g` or `y` flag, so `pattern.test()` is
 * stateless and the same object can be shared by every caller. `use` is what
 * to write instead.
 */
export interface BannedTerm {
  readonly term: string;
  readonly pattern: RegExp;
  readonly use: string;
  readonly note?: string;
}

/**
 * The retired verbs of UI_RULES §3 ("One verb per meaning"), in the same shape
 * as `BANNED_UI_TERMS` but a separate list: the rule-14 banned line does not
 * name them, so a lint opts into this list on its own. Each takes its
 * inflections (reload, reloads, reloaded, reloading).
 */
export const RETIRED_VERB_TERMS: readonly BannedTerm[] = [
  {
    term: "Reload",
    pattern: /\breload(?:s|ed|ing)?\b/i,
    use: "Refresh",
    note: "Retired verb. Refresh re-reads local state.",
  },
  {
    term: "Recheck",
    pattern: /\brecheck(?:s|ed|ing)?\b/i,
    use: "Rescan",
    note: "Retired verb. Rescan re-runs a scanner.",
  },
  {
    term: "Retry",
    pattern: /\bretry(?:ing)?\b|\bretries\b|\bretried\b/i,
    use: "Try again",
    note: "Retired verb. Try again is offered only after a failure.",
  },
  {
    term: "Launch",
    pattern: /\blaunch(?:es|ed|ing)?\b/i,
    use: "Run (a process) or Open (a place)",
    note: 'Retired verb. Over-matches the noun in prose such as "the launch screen".',
  },
  {
    term: "Organise",
    pattern: /\borganis(?:e|es|ed|ing)\b/i,
    use: "Organize",
    note: "US spelling.",
  },
];

/**
 * Banned in user-visible strings (UI_RULES §3). Case-insensitive unless the
 * case is the point (MANVI vs Manvi, lowercase `devmap` vs DevMap).
 */
export const BANNED_UI_TERMS: readonly BannedTerm[] = [
  {
    term: "code graph",
    pattern: /\bcode graph\b/i,
    use: "code map",
  },
  {
    term: "repo map",
    pattern: /\brepo map\b/i,
    use: "code map",
  },
  {
    term: "codeintel",
    pattern: /\bcodeintel\b/i,
    use: "code map",
    note: "Identifiers such as codeintelStore have no word boundary after the stem and do not match.",
  },
  {
    term: "devmap",
    pattern: /(?<![\w./-])devmap(?![\w/-])/,
    use: "code map",
    note: "Case-sensitive: lowercase only. DevMap may name the CLI in Settings → Agents, which the lint cannot see. Paths (devmap/…, .devmap) are excluded; a CLI command in a code-formatted span is not and needs a human look.",
  },
  {
    term: "MANVI",
    pattern: /\bMANVI\b/,
    use: "Manvi",
    note: "Case-sensitive: Manvi is correct and does not match.",
  },
  {
    term: "harness",
    pattern: /(?<![\w./-])harness(?:es)?(?![\w/-]|\.\w)/i,
    use: "checks (as in GitPulse's checks)",
    note: 'Banned only as a UI noun. The lint cannot read grammar: it skips path-shaped uses ("harness/foo.html", "/harness", "test-harness") but cannot tell a noun from a verb ("to harness the power") or a code-formatted name. Expect to review hits by hand.',
  },
  {
    term: "sidecar",
    pattern: /\bsidecars?\b/i,
    use: "checks",
  },
  {
    term: "posture",
    pattern: /\bposture\b/i,
    use: "checks",
  },
  {
    term: "attempt",
    pattern: /\battempts?\b/i,
    use: "run",
    note: 'Noun forms only ("attempted" and "attempting" are not matched). Over-matches the verb in prose.',
  },
  {
    term: "lane",
    pattern: /\blanes?\b/i,
    use: "worktree",
    note: "An agents' internal term. Over-matches prose about roads and layouts.",
  },
  {
    term: "linked checkout",
    pattern: /\b(?:linked|main|shared|separate|other|another) checkouts?\b/i,
    use: "worktree",
    note: 'Narrow on purpose: bare "checkout" is also the git verb and is not banned.',
  },
  {
    term: "ticket",
    pattern: /\btickets?\b/i,
    use: "task",
  },
  {
    term: "repo",
    pattern: /\brepos?\b/i,
    use: "repository",
    note: "Fine in code and identifiers.",
  },
  {
    term: "project",
    pattern: /\bprojects?\b/i,
    use: "repository",
    note: "Over-matches a GitHub Project or a project in the user's own data. Review hits.",
  },
  {
    term: "Preferences",
    pattern: /\bPreferences\b/,
    use: "Settings",
    note: "Case-sensitive: the capitalized title. Lowercase prose is not matched.",
  },
  {
    term: "Options",
    pattern: /\bOptions\b/,
    use: "Settings",
    note: "Case-sensitive: the capitalized title. Over-matches a legitimate heading such as a menu's Options group; review hits.",
  },
  {
    term: "min rung",
    pattern: /\bmin(?:imum)? rungs?\b/i,
    use: "Describe what is required in the user's words",
    note: "Internal Manvi ladder term.",
  },
  {
    term: "rung",
    pattern: /\brungs?\b/i,
    use: "Describe what is required in the user's words",
    note: "Internal Manvi ladder term.",
  },
  {
    term: "ledger record",
    pattern: /\bledger records?\b/i,
    use: "Name what the record says (a commit, a run, a change)",
    note: "Matches ledger record and ledger records.",
  },
  {
    term: "generation N",
    pattern: /\bgeneration #?\d+\b/i,
    use: "Say how stale it is (for example, 3 commits behind)",
    note: "A bare word generation (as in code generation) is not matched.",
  },
  {
    term: "is_fresh",
    pattern: /\bis_fresh\b/,
    use: "Say whether it is up to date",
  },
  {
    term: "snake_case identifier",
    pattern: /\b[a-z][a-z0-9]*(?:_[a-z0-9]+)+\b/,
    use: "The label the user would use",
    note: "Over-matches anywhere identifiers are legitimately shown: a path, a branch name, a code span, a user-supplied name. Meant for string literals and markup text, never for raw source lines.",
  },
  {
    term: "successfully",
    pattern: /\bsuccessfully\b/i,
    use: "Drop it. Say what happened.",
  },
  {
    term: "Oops",
    pattern: /\boops\b/i,
    use: "Say what happened and what the user can do.",
  },
];

/**
 * The voice and spelling rules of UI_RULES §3 that a pattern can approximate,
 * in the same shape as `BANNED_UI_TERMS` but a separate list: they are not
 * terms in rule 14's sense, so a lint opts into this list on its own.
 */
export const VOICE_TERMS: readonly BannedTerm[] = [
  {
    term: "please",
    pattern: /\bplease\b/i,
    use: "Drop it. State the step.",
    note: "Voice rule: no please.",
  },
  {
    term: "exclamation mark",
    pattern: /(?<![\w/-])[A-Za-z][A-Za-z']*!(?=\s|$|["'<])|\)!(?=\s|$|["'<])/,
    use: "A period",
    note: "Voice rule: no exclamation marks. Matches a plain word (letters and apostrophes) or a closing parenthesis followed by ! and then a space, end or quote, so !==, !x, {#if !a} and Tailwind's important suffix on a class (a border-color utility ending in /60!) do not match. A TypeScript non-null assertion on a plain identifier followed by a space (value! ) would.",
  },
  {
    term: "British spelling",
    pattern:
      /\b(?:colour|behaviour|favourite|centre|analyse|licence|(?:customi|initiali|normali|recogni|synchroni|visuali|summari|optimi|prioriti|minimi|maximi)s(?:e|es|ed|ing|ation))\w*/i,
    use: "The US spelling (color, behavior, organize)",
    note: "A stem list, not a dictionary. It catches the usual offenders only.",
  },
];

/**
 * The terms of `terms` (default `BANNED_UI_TERMS`) that match `text`, in list
 * order. Pure and stateless. An empty result means no listed pattern matched,
 * which is weaker than "the copy is clean": see the note on each pattern.
 */
export function findBannedTerms(text: string, terms: readonly BannedTerm[] = BANNED_UI_TERMS): BannedTerm[] {
  return terms.filter((banned) => banned.pattern.test(text));
}
