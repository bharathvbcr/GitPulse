import { get } from "svelte/store";
import { beforeEach, describe, expect, it } from "vitest";
import {
  chooseRecipient,
  forgetRecipients,
  pickRecipient,
  publishRecipients,
  recipientChoice,
  recipientDirectory,
  recipientLabel,
  recipientView,
  type RecipientCandidate,
} from "./recipient";

const main = "/work/repo";
const wt = "/work/repo-wt/feature-x";
const tab = (id: string, launcher: RecipientCandidate["launcher"], checkout = main, extra: Partial<RecipientCandidate> = {}): RecipientCandidate => ({
  id, launcher, checkout, live: true, visible: false, ...extra,
});

beforeEach(() => {
  recipientDirectory.set({});
  recipientChoice.set({});
});

describe("pickRecipient", () => {
  it("is null when nothing can take text: no sessions, only ended ones, a browser, a file pane", () => {
    expect(pickRecipient([], { checkout: main })).toBeNull();
    expect(pickRecipient([tab("a", "claude", main, { live: false })], { checkout: main })).toBeNull();
    expect(pickRecipient([tab("b", "browser"), tab("f", "file")], { checkout: main })).toBeNull();
  });

  it("prefers a session in the file's own checkout over a visible agent in another", () => {
    const candidates = [tab("other", "claude", wt, { visible: true }), tab("here", "shell", main)];
    expect(pickRecipient(candidates, { checkout: main })?.id).toBe("here");
    expect(pickRecipient(candidates, { checkout: wt })?.id).toBe("other");
  });

  it("within a checkout takes an agent before a shell and a visible session before a hidden one", () => {
    expect(pickRecipient([tab("s", "shell", main, { visible: true }), tab("c", "claude")], { checkout: main })?.id).toBe("c");
    expect(pickRecipient([tab("c1", "claude"), tab("c2", "codex", main, { visible: true })], { checkout: main })?.id).toBe("c2");
  });

  it("falls back to any checkout when none matches, ranking the same way", () => {
    expect(pickRecipient([tab("s", "shell", wt, { visible: true }), tab("c", "claude", wt)], { checkout: main })?.id).toBe("c");
  });

  it("keeps tab order among equals", () => {
    expect(pickRecipient([tab("one", "claude"), tab("two", "claude")], { checkout: main })?.id).toBe("one");
  });

  it("honours the person's pick over the rule, until that session is no longer eligible", () => {
    const candidates = [tab("c", "claude"), tab("s", "shell", wt)];
    expect(pickRecipient(candidates, { checkout: main, stickyId: "s" })?.id).toBe("s");
    const ended = [tab("c", "claude"), tab("s", "shell", wt, { live: false })];
    expect(pickRecipient(ended, { checkout: main, stickyId: "s" })?.id).toBe("c");
  });

  it("matches a checkout by identity, not by spelling", () => {
    expect(pickRecipient([tab("a", "claude", "/work/repo/"), tab("b", "claude", wt)], { checkout: "/work/repo" })?.id).toBe("a");
  });
});

describe("recipientView and label", () => {
  it("lists every eligible session so a picker has something to offer", () => {
    const view = recipientView([tab("a", "claude"), tab("b", "shell", wt), tab("x", "claude", main, { live: false })], { checkout: main });
    expect(view.options.map((c) => c.id)).toEqual(["a", "b"]);
    expect(view.chosen?.id).toBe("a");
  });

  it("names the session and its checkout", () => {
    expect(recipientLabel(tab("a", "claude", wt))).toBe("claude · feature-x");
  });
});

describe("the choice for a repository", () => {
  it("sticks while the session lives and is dropped the moment it is gone", () => {
    publishRecipients(main, [tab("a", "claude"), tab("b", "shell")]);
    chooseRecipient(main, "b");
    expect(get(recipientChoice)[main]).toBe("b");
    publishRecipients(main, [tab("a", "claude"), tab("b", "shell")]);
    expect(get(recipientChoice)[main]).toBe("b");
    publishRecipients(main, [tab("a", "claude"), tab("b", "shell", main, { live: false })]);
    expect(get(recipientChoice)[main]).toBeUndefined();
  });

  it("is kept per repository", () => {
    chooseRecipient("/one", "a");
    chooseRecipient("/two", "b");
    chooseRecipient("/one", null);
    expect(get(recipientChoice)).toEqual({ "/two": "b" });
  });

  it("is forgotten with the panel that published the sessions", () => {
    publishRecipients(main, [tab("a", "claude")]);
    chooseRecipient(main, "a");
    forgetRecipients(main);
    expect(get(recipientDirectory)[main]).toBeUndefined();
    expect(get(recipientChoice)[main]).toBeUndefined();
  });
});
