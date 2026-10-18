import { describe, expect, it } from "vitest";
import { classifierMatches } from "./error-classifier-scan";

/**
 * Adversarial review of the error-classifier scan (b17/5c1313c9-adv). Each case
 * is a text classifier on an error message that the scan must report.
 *
 * The scan follows where a value comes from rather than what it is called (see
 * error-classifier-scan.ts), so every evasion the review found is a plain `it`
 * now. The one it still cannot see is listed in `STILL_UNSEEN` as `it.fails`:
 * when the scan learns it, that case flips red, which is the prompt to move it.
 */
const caught: Array<[string, string]> = [
  ["renamed variable (stderr), by the phrase", "const f = (stderr: string) => stderr.includes('Refresh the stash list');"],
  ["renamed variable (out), by the phrase", "const f = (out: string) => out.startsWith('Git does not know who you are');"],
  ["renamed variable (body), by the phrase", "const f = (body: string) => body.indexOf('deferred under load') >= 0;"],
  ["cast receiver, by the marker", "const f = (x: unknown) => (x as string).includes('REPOSITORY_TRUST_REQUIRED');"],
  ["String(e) receiver", "const f = (e: unknown) => String(e).includes('REPOSITORY_TRUST_REQUIRED');"],
  ["formatError(e) receiver", "const f = (e: unknown) => formatError(e).includes('timed out');"],
  ["regex held in a const", "const RE = /Refresh the stash list/i; const f = (message: string) => RE.test(message);"],
  ["new RegExp(...).test", "const f = (message: string) => new RegExp('Refresh the stash').test(message);"],
  ["RegExp(...).exec", "const f = (message: string) => /Refresh/.exec(message) !== null;"],
  ["matchAll", "const f = (message: string) => [...message.matchAll(/stash/g)].length > 0;"],
  ["split", "const f = (message: string) => message.split(': ')[0] === 'REPOSITORY_TRUST_REQUIRED';"],
  ["slice compare", "const f = (message: string) => message.slice(0, 25) === 'REPOSITORY_TRUST_REQUIRED';"],
  ["template literal compare", "const f = (message: string) => message === `Git does not know who you are`;"],
  ["equality on a message", "const f = (error: string) => error === 'Refresh the stash list and try again.';"],
  ["switch on a message", "const f = (message: string) => { switch (message) { case 'x': return 1; default: return 0; } };"],
  ["computed method name", "const f = (message: string) => message['includes']('stash');"],
  ["Function.call", "const f = (message: string) => String.prototype.includes.call(message, 'stash');"],
  ["destructured alias", "const f = (e: { message: string }) => { const { message: m } = e; return m.includes('stash'); };"],
  ["replace-then-compare", "const f = (message: string) => message.replace('x', '') === '';"],
  // Derived from a catch binding or `.message`, under any name.
  ["catch binding under any name", "async function f() { try { await g(); } catch (boom) { return boom.includes('stash'); } }"],
  ["String() of a catch binding", "async function f() { try { await g(); } catch (z) { return String(z).startsWith('nope'); } }"],
  ["catch binding's .message in a variable", "async function f() { try { await g(); } catch (z) { const t = z.message; return t.endsWith('later'); } }"],
  ["a chain of derived variables", "const f = (res: { message: string }) => { const a = res.message; const b = a.trim(); const c = b.toLowerCase(); return c.includes('x'); };"],
  [".message of anything", "const f = (res: { message: string }) => res.message.includes('nope');"],
  ["catch binding compared with a literal", "async function f() { try { await g(); } catch (z) { return z === 'busy'; } }"],
  ["destructured catch binding", "async function f() { try { await g(); } catch ({ message: m }) { return m.includes('x'); } }"],
  ["a derived value compared in a switch", "async function f() { try { await g(); } catch (z) { switch (String(z)) { case 'a': return 1; default: return 0; } } }"],
  ["a regex that is a sentence, on any value", "const f = (v: string) => /Refresh the stash list/i.test(v);"],
];

/** Code that reads like the above and is not a classifier of an error. */
const quiet: Array<[string, string]> = [
  ["membership of a list", "const ok = names.includes(name) && /^[a-f0-9]+$/.test(oid);"],
  ["emptiness of a message", "const f = (message: string) => message.trim() === '' || message === '';"],
  ["a typeof check", "const f = (error: unknown) => typeof error === 'string';"],
  ["an instanceof check", "const f = (error: unknown) => error instanceof Error;"],
  ["a kind read from the guard", "const f = (error: unknown) => ipcErrorKind(error) === 'stale_stash';"],
  ["text that never came from an error", "const f = (path: string) => path.startsWith('src/') && path.endsWith('.ts');"],
];

/**
 * What the scan cannot see, and why. A bare `string` parameter has no name or
 * origin that says it is an error, and its literal is one word or an
 * identifier, so nothing in the code marks the match as a message classifier.
 */
const STILL_UNSEEN: Array<[string, string]> = [
  ["array some over a plain string parameter", "const f = (m: string) => ['a', 'b'].some((p) => m.includes(p));"],
];

describe("error-classifier scan evasions", () => {
  for (const [name, source] of caught) {
    it(`reports: ${name}`, () => {
      expect(classifierMatches("x.ts", source)).not.toEqual([]);
    });
  }

  for (const [name, source] of quiet) {
    it(`leaves alone: ${name}`, () => {
      expect(classifierMatches("x.ts", source)).toEqual([]);
    });
  }

  for (const [name, source] of STILL_UNSEEN) {
    it.fails(`does not yet report: ${name}`, () => {
      expect(classifierMatches("x.ts", source)).not.toEqual([]);
    });
  }

  it("reads a Svelte component's script", () => {
    expect(classifierMatches("x.svelte", `<script lang="ts">const f = (error: unknown) => String(error).includes("a stale list");</script>`))
      .toHaveLength(1);
  });
});
