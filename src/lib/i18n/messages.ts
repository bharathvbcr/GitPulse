/**
 * The string catalogue: every user-visible string of an extracted subsystem,
 * keyed by `area.name`. English is the source locale and the fallback for any
 * key another locale has not translated. `messages.test.ts` fails when a key
 * here is referenced nowhere, so the catalogue cannot accumulate dead strings.
 *
 * Pilot subsystem: the optional-tools setup wizard chrome.
 */
export const EN = {
  "setup.dialogLabel": "Optional tools setup",
  "setup.title": "Set up optional tools",
  "setup.subtitle": "Map and policy are optional",
  "setup.close": "Close",
  "setup.step.explain": "Why",
  "setup.step.source": "Source",
  "setup.step.preflight": "Check",
  "setup.step.install": "Install",
  "setup.step.verify": "Verify",
  "setup.step.done": "Done",
  "setup.checking": "Checking prerequisites…",
  "setup.complete": "Setup complete for this session.",
} as const;

export type MessageKey = keyof typeof EN;
export type Catalogue = Readonly<Record<MessageKey, string>>;
