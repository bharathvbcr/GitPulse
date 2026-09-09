import { describe, expect, it } from "vitest";
import { remoteWebsite } from "./remoteWebsite";

describe("remote website handoff", () => {
  it.each([
    ["git@github.com:team/repo.git", "https://github.com/team/repo"],
    ["ssh://git@example.com:2222/team/repo.git", "https://example.com/team/repo"],
    ["git://example.com/team/repo.git", "https://example.com/team/repo"],
    ["https://user:secret@example.com:8443/team/repo.git?token=secret#secret", "https://example.com:8443/team/repo"],
    ["http://localhost:3000/team/repo.git", "http://localhost:3000/team/repo"],
  ])("converts %s without sending credentials", (input, expected) => expect(remoteWebsite(input)).toBe(expected));
  it.each([null, "", "/tmp/repo", "C:/repo", "file:///repo", "javascript:alert(1)", "https://example.com", "git@host:repo\nspoof", "x".repeat(20000)])(
    "refuses a missing, local, malformed or unsupported address", (input) => expect(remoteWebsite(input)).toBeNull(),
  );
});
