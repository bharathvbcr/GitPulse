import { describe, expect, it } from "vitest";
import {
  matchesCommit,
  parseFilterQuery,
  queryNeedsServerFetch,
  type ParsedFilterQuery,
} from "./parseQuery";

describe("parseFilterQuery", () => {
  it("parses author, conventional type, sha, and free text", () => {
    const parsed = parseFilterQuery("author:Alice feat: oauth sha:abc123");
    expect(parsed.author).toBe("alice");
    expect(parsed.commitType).toBe("feat");
    expect(parsed.sha).toBe("abc123");
    expect(parsed.text).toBe("oauth");
  });

  it("matches commits by author and type", () => {
    const filter = parseFilterQuery("author:alice feat:");
    expect(
      matchesCommit(
        {
          id: "aaa111",
          summary: "feat: add login",
          author_name: "Alice",
          author_email: "alice@example.com",
        },
        filter
      )
    ).toBe(true);
    expect(
      matchesCommit(
        {
          id: "aaa111",
          summary: "fix: typo",
          author_name: "Alice",
          author_email: "alice@example.com",
        },
        filter
      )
    ).toBe(false);
  });

  it.each([
    {
      name: "valid type via type: prefix builds the commitType predicate",
      query: "type:feat",
      expected: { commitType: "feat", text: "" },
    },
    {
      name: "invalid type via type: prefix falls back to free text",
      query: "type:zzz",
      expected: { text: "type:zzz" },
    },
    {
      name: "valid bare suffix keeps conventional filtering",
      query: "chore:",
      expected: { commitType: "chore", text: "" },
    },
    {
      name: "invalid bare suffix stays free text",
      query: "zzz:",
      expected: { text: "zzz:" },
    },
    {
      name: "empty query parses to bare text",
      query: "",
      expected: { text: "" },
    },
    {
      name: "whitespace-only query parses to bare text",
      query: "   \t\n  ",
      expected: { text: "" },
    },
    {
      name: "duplicate keys keep the last value",
      query: "author:a author:b feat: chore:",
      expected: { author: "b", commitType: "chore", text: "" },
    },
    {
      name: "regex metachars are stored literally, never compiled",
      query: "path:(src|lib)* author:^a$.*[x]",
      expected: { path: "(src|lib)*", author: "^a$.*[x]", text: "" },
    },
    {
      name: "unicode values are lowercased and kept",
      query: "author:ÅÄÖ type:fix",
      expected: { author: "åäö", commitType: "fix", text: "" },
    },
    {
      name: "very long tokens survive intact",
      query: `sha:${"a".repeat(10_000)}`,
      expected: { sha: "a".repeat(10_000), text: "" },
    },
  ] as Array<{ name: string; query: string; expected: Partial<ParsedFilterQuery> }>)(
    "$name",
    ({ query, expected }) => {
      expect(parseFilterQuery(query)).toMatchObject(expected);
    }
  );

  it("an invalid type: token filters as plain text instead of an impossible predicate", () => {
    expect(parseFilterQuery("type:zzz").commitType).toBeUndefined();
    expect(parseFilterQuery("zzz:").commitType).toBeUndefined();
    const filter = parseFilterQuery("type:zzz");
    const row = {
      id: "abc123",
      summary: "docs mention the literal type:zzz token here",
      author_name: "A",
      author_email: "a@example.com",
    };
    expect(matchesCommit(row, filter)).toBe(true);
  });
});

describe("queryNeedsServerFetch", () => {
  it("needs git only when a path: token walks file history", () => {
    expect(queryNeedsServerFetch("path:src/auth")).toBe(true);
    expect(queryNeedsServerFetch("author:ada path:src/ fix:")).toBe(true);
  });

  it("client-side filtering covers every other operator", () => {
    expect(queryNeedsServerFetch("")).toBe(false);
    expect(queryNeedsServerFetch("oauth token")).toBe(false);
    expect(queryNeedsServerFetch("author:ada sha:abc feat:")).toBe(false);
  });
});
