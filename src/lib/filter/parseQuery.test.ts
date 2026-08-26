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

  it("accepts all four conventional header shapes, matching the backend", () => {
    // Parity contract with CommitFilter::matches_commit (filter.rs): plain,
    // scoped, and both breaking variants. Missing the `!` shapes here made
    // the client drop breaking-change commits the backend kept and solved
    // around — every divergence shifts row indices under solved lanes.
    const filter = parseFilterQuery("feat:");
    const rowWith = (summary: string) => ({
      id: "aaa111",
      summary,
      author_name: "Alice",
      author_email: "alice@example.com",
    });
    expect(matchesCommit(rowWith("feat: add login"), filter)).toBe(true);
    expect(matchesCommit(rowWith("feat(scope): add login"), filter)).toBe(true);
    expect(matchesCommit(rowWith("feat!: breaking change"), filter)).toBe(true);
    expect(matchesCommit(rowWith("feat(scope)!: breaking change"), filter)).toBe(true);
    expect(matchesCommit(rowWith("fix: typo"), filter)).toBe(false);
  });

  it("free text searches summary, name, EMAIL, and id as a word conjunction", () => {
    const filter = parseFilterQuery("oauth flow");
    const rowWith = (over: Partial<{ id: string; summary: string; author_name: string; author_email: string }>) => ({
      id: "aaa111",
      summary: "no keywords here",
      author_name: "Alice",
      author_email: "alice@example.com",
      ...over,
    });
    // Word-AND, not contiguous phrase: backend parity (filter.rs).
    expect(matchesCommit(rowWith({ summary: "fix: flow for oauth" }), filter)).toBe(true);
    // Email is part of the haystack on the backend; the client must agree.
    expect(matchesCommit(rowWith({ author_email: "oauth@flow.dev" }), filter)).toBe(true);
    // One missing word fails the conjunction.
    expect(matchesCommit(rowWith({ summary: "oauth only" }), filter)).toBe(false);
  });

  it.each([
    {
      name: "valid type via type: prefix builds the commitType predicate",
      query: "type:feat",
      expected: { commitType: "feat", text: "" },
    },
    {
      name: "non-conventional type: values stay predicates, matching the backend filter",
      query: "type:zzz",
      expected: { commitType: "zzz", text: "" },
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

  it("type: accepts any value, matching the backend filter exactly", () => {
    // Parity contract (analyzer/filter.rs): CommitFilter::parse takes any
    // non-empty type: value. The old client-only conventional-set gate made
    // `type:wip` a free-text token here and a commit-type predicate on the
    // server — divergent predicates shift which rows survive, corrupting
    // solved lane geometry under client-side filtering.
    expect(parseFilterQuery("type:zzz").commitType).toBe("zzz");
    expect(parseFilterQuery("type:wip").commitType).toBe("wip");
    // A BARE conventional token still routes through the conventional set.
    expect(parseFilterQuery("zzz:").commitType).toBeUndefined();
    const filter = parseFilterQuery("type:zzz");
    const matching = {
      id: "abc123",
      summary: "zzz: something",
      author_name: "A",
      author_email: "a@example.com",
    };
    const other = {
      id: "abc124",
      summary: "docs mention the literal type:zzz token here",
      author_name: "A",
      author_email: "a@example.com",
    };
    expect(matchesCommit(matching, filter)).toBe(true);
    expect(matchesCommit(other, filter)).toBe(false);
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
