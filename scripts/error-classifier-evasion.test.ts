import { describe, expect, it } from "vitest";
import { messageMatches } from "./error-classifier-contract.test";

/**
 * Adversarial review of scripts/error-classifier-contract.test.ts
 * (b17/5c1313c9-adv). Each case is a text classifier on an error message that
 * the scan SHOULD report. `it.fails` marks the ones it misses today: when the
 * scan is hardened a case starts passing and `it.fails` flips red, which is the
 * prompt to turn it into a plain `it`.
 */
const evasions: Array<[string, string]> = [
  ["renamed variable (stderr)", "const f = (stderr: string) => stderr.includes('Refresh the stash list');"],
  ["renamed variable (out)", "const f = (out: string) => out.startsWith('Git does not know');"],
  ["renamed variable (body)", "const f = (body: string) => body.indexOf('deferred under load') >= 0;"],
  ["cast receiver", "const f = (x: unknown) => (x as string).includes('REPOSITORY_TRUST_REQUIRED');"],
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
  ["array some", "const f = (m: string) => ['a', 'b'].some((p) => m.includes(p));"],
  ["replace-then-compare", "const f = (message: string) => message.replace('x', '') === '';"],
];

describe("error-classifier scan evasions", () => {
  for (const [name, source] of evasions) {
    it.fails(`reports: ${name}`, () => {
      expect(messageMatches("x.ts", source)).not.toEqual([]);
    });
  }
});
