import { describe, expect, it } from "vitest";
import { formatError } from "../formatError";
import {
  DETAIL_MAX_CHARS,
  OBJECT_MAX_CHARS,
  confirmDestructive,
  describeFailure,
  toOneLine,
  unknownState,
} from "./describeFailure";
import { BANNED_UI_TERMS, RETIRED_VERB_TERMS, VOICE_TERMS, findBannedTerms } from "./vocabulary";

const fetchBranches = { verb: "fetch", object: "branches" } as const;

describe("describeFailure headline", () => {
  it("is exactly Couldn't <verb> <object>. and always offers Try again", () => {
    const copy = describeFailure(fetchBranches, new Error("network down"));
    expect(copy.headline).toBe("Couldn't fetch branches.");
    expect(copy.retryLabel).toBe("Try again");
    expect(copy.detail).toBe("network down");
  });

  it("never carries backend text, whatever the error says", () => {
    const hostile = "Failed to fetch: ENOTFOUND github.com";
    const copy = describeFailure({ verb: "open", object: "the worktree" }, hostile);
    expect(copy.headline).toBe("Couldn't open the worktree.");
    expect(copy.headline).not.toContain("ENOTFOUND");
    expect(copy.detail).toBe(hostile);
  });

  it("lowercases the verb, keeps the object's own case, and drops stray end punctuation", () => {
    expect(describeFailure({ verb: "Fetch", object: "origin." }, "x").headline).toBe("Couldn't fetch origin.");
    expect(describeFailure({ verb: "  DELETE ", object: "GitHub issues?!" }, "x").headline).toBe(
      "Couldn't delete GitHub issues.",
    );
  });

  it("collapses whitespace and newlines inside the verb and object", () => {
    expect(describeFailure({ verb: "load\n", object: "the\n  diff\tview" }, "x").headline).toBe(
      "Couldn't load the diff view.",
    );
  });

  it("falls back rather than throwing on blank, empty or whitespace-only action parts", () => {
    expect(describeFailure({ verb: "", object: "" }, "x").headline).toBe("Couldn't finish that.");
    expect(describeFailure({ verb: "   ", object: "branches" }, "x").headline).toBe("Couldn't finish branches.");
    expect(describeFailure({ verb: "fetch", object: " \n " }, "x").headline).toBe("Couldn't fetch that.");
    expect(describeFailure({ verb: "...", object: "?!" }, "x").headline).toBe("Couldn't finish that.");
  });

  it("caps a very long object and keeps the ellipsis visible without a trailing period", () => {
    const headline = describeFailure({ verb: "open", object: "x".repeat(500) }, "y").headline;
    expect(Array.from(headline).length).toBe("Couldn't open ".length + OBJECT_MAX_CHARS);
    expect(headline.endsWith("…")).toBe(true);
    expect(headline.startsWith("Couldn't open x")).toBe(true);
  });

  it("keeps unicode objects whole", () => {
    expect(describeFailure({ verb: "open", object: "日本語のリポジトリ 🚀" }, "x").headline).toBe(
      "Couldn't open 日本語のリポジトリ 🚀.",
    );
  });
});

describe("describeFailure detail", () => {
  it("reuses formatError for every rejection shape formatError handles", () => {
    const shapes: unknown[] = [
      "clone failed",
      new Error("db locked"),
      new TypeError("not a function"),
      { message: "rpc failed", code: -1 },
      { code: -1, kind: "io" },
      { b: 2, a: 1 },
      new Map([["k", "v"]]),
      new Set([1, 2]),
      { id: 9007199254740993n },
      42,
      true,
      Symbol("rate limited"),
      undefined,
      null,
      "",
      "   ",
      () => "nope",
    ];
    for (const shape of shapes) {
      expect(describeFailure(fetchBranches, shape).detail, String(shape)).toBe(formatError(shape));
    }
  });

  it("survives hostile errors that make formatError fall back", () => {
    const cyclic: Record<string, unknown> = { a: 1 };
    cyclic.self = cyclic;
    expect(describeFailure(fetchBranches, cyclic).detail).toBe("Unknown error");
    const hostile = {
      get message(): never {
        throw new Error("boom");
      },
    };
    expect(describeFailure(fetchBranches, hostile).detail).toBe("Unknown error");
  });

  it("says Unknown error, not nothing, for an empty rejection", () => {
    expect(describeFailure(fetchBranches, undefined).detail).toBe("Unknown error");
    expect(describeFailure(fetchBranches, "").detail).toBe("Unknown error");
  });

  it("collapses multi-line backend output to one line", () => {
    const err = new Error("fatal: unable to access\n\tremote: denied\r\n  hint: check your token\n\n");
    expect(describeFailure(fetchBranches, err).detail).toBe(
      "fatal: unable to access remote: denied hint: check your token",
    );
  });

  it("strips ANSI colour codes and control characters", () => {
    expect(describeFailure(fetchBranches, "\u001b[31merror\u001b[0m: bad\u0000ref\u0007").detail).toBe("error: bad ref");
  });

  it("caps a very long backend error at the default and ends it with an ellipsis", () => {
    const detail = describeFailure(fetchBranches, new Error("e".repeat(100_000))).detail ?? "";
    expect(Array.from(detail).length).toBe(DETAIL_MAX_CHARS);
    expect(detail.endsWith("…")).toBe(true);
    expect(detail.slice(0, -1)).toBe("e".repeat(DETAIL_MAX_CHARS - 1));
  });

  it("does not cut an error that is exactly at the cap", () => {
    const exact = "z".repeat(DETAIL_MAX_CHARS);
    expect(describeFailure(fetchBranches, exact).detail).toBe(exact);
    expect(describeFailure(fetchBranches, `${exact}z`).detail?.endsWith("…")).toBe(true);
  });

  it("honours a custom cap and never exceeds it", () => {
    const detail = describeFailure(fetchBranches, "abcdefghij", { maxDetailChars: 5 }).detail;
    expect(detail).toBe("abcd…");
  });

  it("treats a nonsense cap as the default instead of emitting nothing", () => {
    const long = "q".repeat(1000);
    for (const maxDetailChars of [0, -3, Number.NaN, Number.POSITIVE_INFINITY]) {
      const detail = describeFailure(fetchBranches, long, { maxDetailChars }).detail ?? "";
      expect(detail.length, String(maxDetailChars)).toBeGreaterThan(0);
      expect(Array.from(detail).length, String(maxDetailChars)).toBeLessThanOrEqual(DETAIL_MAX_CHARS);
    }
  });

  it("never splits a surrogate pair when it cuts", () => {
    const detail = describeFailure(fetchBranches, "🚀".repeat(500), { maxDetailChars: 7 }).detail ?? "";
    expect(detail).toBe(`${"🚀".repeat(6)}…`);
    expect(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])/.test(detail)).toBe(false);
  });

  it("does not trail a space before the ellipsis", () => {
    expect(describeFailure(fetchBranches, "ab cd ef", { maxDetailChars: 6 }).detail).toBe("ab cd…");
  });

  it("does not rewrite the backend text it keeps (no banned-term stripping, no ... to …)", () => {
    expect(describeFailure(fetchBranches, "waiting...").detail).toBe("waiting...");
  });
});

describe("the error template itself is clean copy", () => {
  it("contains no banned term and no three-dot ellipsis", () => {
    const copy = describeFailure(fetchBranches, "x");
    const every = [...BANNED_UI_TERMS, ...RETIRED_VERB_TERMS, ...VOICE_TERMS];
    expect(findBannedTerms(`${copy.headline} ${copy.retryLabel}`, every)).toEqual([]);
    expect(`${copy.headline}${copy.retryLabel}`).not.toContain("...");
  });
});

describe("confirmDestructive", () => {
  it("builds the title, body and danger button from the template", () => {
    expect(
      confirmDestructive({ verb: "Delete", object: "branch feature/x", consequence: "This removes the branch and its 3 unmerged commits" }),
    ).toEqual({
      title: "Delete branch feature/x?",
      body: "This removes the branch and its 3 unmerged commits.",
      confirmLabel: "Delete",
      cancelLabel: "Cancel",
      variant: "danger",
    });
  });

  it("capitalizes a lowercase verb and keeps an existing sentence end", () => {
    const copy = confirmDestructive({ verb: "discard", object: "12 changes", consequence: "They can't be recovered." });
    expect(copy.title).toBe("Discard 12 changes?");
    expect(copy.confirmLabel).toBe("Discard");
    expect(copy.body).toBe("They can't be recovered.");
    expect(confirmDestructive({ verb: "Remove", object: "a", consequence: "Gone…" }).body).toBe("Gone…");
    expect(confirmDestructive({ verb: "Remove", object: "a", consequence: "Sure?" }).body).toBe("Sure?");
  });

  it("flattens a multi-line consequence and caps a huge one", () => {
    const flat = confirmDestructive({ verb: "Delete", object: "x", consequence: "Line one\nline two" });
    expect(flat.body).toBe("Line one line two.");
    const capped = confirmDestructive({ verb: "Delete", object: "x", consequence: "w".repeat(10_000) });
    expect(Array.from(capped.body).length).toBeLessThanOrEqual(DETAIL_MAX_CHARS + 1);
  });

  it("strips a question mark the caller already put on the object", () => {
    expect(confirmDestructive({ verb: "Delete", object: "the worktree?", consequence: "Gone" }).title).toBe(
      "Delete the worktree?",
    );
  });

  it("throws on a blank verb, object or consequence rather than showing an empty dialog", () => {
    expect(() => confirmDestructive({ verb: "", object: "x", consequence: "y" })).toThrow(TypeError);
    expect(() => confirmDestructive({ verb: "Delete", object: "  ", consequence: "y" })).toThrow(TypeError);
    expect(() => confirmDestructive({ verb: "Delete", object: "x", consequence: "\n" })).toThrow(TypeError);
    expect(() => confirmDestructive({ verb: "?", object: "x", consequence: "y" })).toThrow(TypeError);
  });

  it("keeps unicode objects whole", () => {
    expect(confirmDestructive({ verb: "Delete", object: "ブランチ 🚀", consequence: "元に戻せません" }).title).toBe(
      "Delete ブランチ 🚀?",
    );
  });

  it("uses only live verbs' own labels: Cancel stays Cancel and the variant is never primary", () => {
    const copy = confirmDestructive({ verb: "Delete", object: "x", consequence: "y" });
    expect(copy.cancelLabel).toBe("Cancel");
    expect(copy.variant).toBe("danger");
  });
});

describe("unknownState", () => {
  it("builds Couldn't check <object> with the reason and a Rescan action", () => {
    expect(unknownState("secrets", "The scanner timed out after 30 s.")).toEqual({
      title: "Couldn't check secrets",
      reason: "The scanner timed out after 30 s.",
      actionLabel: "Rescan",
    });
  });

  it("never reads as clean: no 'clean', 'none', '0' or 'no issues' in the title", () => {
    const { title } = unknownState("coverage", "x");
    expect(title).not.toMatch(/clean|none|no issues|\b0\b/i);
  });

  it("is honest when the reason is blank or the object is blank", () => {
    expect(unknownState("coverage", "").reason).toBe("No reason was reported.");
    expect(unknownState("coverage", " \n ").reason).toBe("No reason was reported.");
    expect(unknownState("", "x").title).toBe("Couldn't check this");
    expect(unknownState("coverage.", "x").title).toBe("Couldn't check coverage");
  });

  it("flattens and caps the reason", () => {
    expect(unknownState("x", "a\nb").reason).toBe("a b");
    const reason = unknownState("x", "r".repeat(5000)).reason;
    expect(Array.from(reason).length).toBe(DETAIL_MAX_CHARS);
    expect(reason.endsWith("…")).toBe(true);
  });
});

describe("toOneLine", () => {
  it("returns the empty string for empty and whitespace-only input", () => {
    expect(toOneLine("")).toBe("");
    expect(toOneLine(" \n\t ")).toBe("");
  });

  it("is idempotent", () => {
    const once = toOneLine("a\n\nb   c" + "d".repeat(1000));
    expect(toOneLine(once)).toBe(once);
  });

  it("handles a cap of one", () => {
    expect(toOneLine("abc", 1)).toBe("…");
  });
});
