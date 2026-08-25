import { describe, expect, it } from "vitest";
import {
  matchesCommit,
  parseFilterQuery,
  queryNeedsServerFetch,
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
