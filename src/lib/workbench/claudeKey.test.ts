import { describe, expect, it } from "vitest";
import {
  claudeCredentialProblem,
  claudeCredentialStatus,
  claudeKeyProblem,
  claudeModelLabel,
} from "./claudeKey";

describe("claudeModelLabel", () => {
  it("shortens a Claude model id and leaves anything else alone", () => {
    expect(claudeModelLabel("claude-sonnet-5-5")).toBe("Sonnet 5.5");
    expect(claudeModelLabel("claude-opus-5-5")).toBe("Opus 5.5");
    expect(claudeModelLabel("custom-model")).toBe("custom-model");
  });
});

describe("claudeCredentialProblem", () => {
  const account = { name: "main", workspace: "wrkspc_x", source: "default" };
  it("is null when an account or a saved key can run Manvi", () => {
    expect(claudeCredentialProblem({ keySaved: false, account, accountError: null })).toBeNull();
    expect(claudeCredentialProblem({ keySaved: true, account: null, accountError: null })).toBeNull();
  });
  it("names an unreadable registry even when a key is saved", () => {
    expect(claudeCredentialProblem({ keySaved: true, account: null, accountError: "no launcher" })).toContain("no launcher");
  });
  it("names the absence of both", () => {
    expect(claudeCredentialProblem({ keySaved: false, account: null, accountError: null })).toContain("No Claude credential");
  });
});

describe("claudeCredentialStatus", () => {
  it("reads the registry account with its workspace", () => {
    expect(
      claudeCredentialStatus({
        key_saved: true,
        account: { name: "main", workspace: "wrkspc_01Test", source: "default" },
        account_error: null,
      }),
    ).toEqual({
      keySaved: true,
      account: { name: "main", workspace: "wrkspc_01Test", source: "default" },
      accountError: null,
    });
  });

  it("keeps a broken registry distinct from no registry", () => {
    const broken = claudeCredentialStatus({ key_saved: false, account: null, account_error: "no launcher" });
    expect(broken.accountError).toBe("no launcher");
    const absent = claudeCredentialStatus({ key_saved: false, account: null, account_error: null });
    expect(absent).toEqual({ keySaved: false, account: null, accountError: null });
  });

  it("refuses a malformed answer instead of reading it as no key", () => {
    for (const raw of [false, null, {}, { key_saved: "yes" }, { key_saved: true, account: { workspace: "w" } }]) {
      expect(() => claudeCredentialStatus(raw), JSON.stringify(raw)).toThrow();
    }
  });
});

describe("claudeKeyProblem", () => {
  it("accepts an Anthropic key, trimmed", () => {
    expect(claudeKeyProblem("  sk-ant-api03-Abc_def-0123456789  ")).toBeNull();
  });

  it("refuses what the backend would refuse", () => {
    for (const raw of [
      "",
      "   ",
      "sk-proj-0123456789abcdefghij",
      "sk-ant-short",
      "sk-ant-api03-has space-0123456789",
      "sk-ant-api03-0123456789abcdef;rm",
      `sk-ant-${"a".repeat(300)}`,
    ]) {
      expect(claudeKeyProblem(raw), raw).not.toBeNull();
    }
  });
});
