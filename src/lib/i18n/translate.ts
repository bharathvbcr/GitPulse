import { EN, type Catalogue, type MessageKey } from "./messages";

export type MessageParams = Readonly<Record<string, string | number>>;

/**
 * Build a lookup for one locale. A key the locale does not translate falls
 * back to English, so a partial translation degrades to English instead of
 * showing a key. `{name}` placeholders are replaced from `params`; a
 * placeholder with no parameter is left visible rather than silently dropped.
 */
export function createTranslator(overrides: Partial<Catalogue> = {}) {
  return function t(key: MessageKey, params?: MessageParams): string {
    const template = overrides[key] ?? EN[key];
    if (!params) return template;
    return template.replace(/\{(\w+)\}/g, (placeholder, name: string) =>
      Object.hasOwn(params, name) ? String(params[name]) : placeholder,
    );
  };
}

/** The app's translator. English is the only locale GitPulse ships today. */
export const t = createTranslator();
