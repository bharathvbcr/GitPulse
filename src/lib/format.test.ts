import { describe, expect, it } from "vitest";
import { formatDate, formatRelativeTime, shortHash, signatureLabel } from "./format";
import type { CommitSignatureCheck } from "./pulse/types";

describe("formatRelativeTime", () => {
  const NOW = 1_700_000_100;

  it("returns just now inside the first minute", () => {
    expect(formatRelativeTime(NOW, NOW)).toBe("just now");
    expect(formatRelativeTime(NOW - 1, NOW)).toBe("just now");
    expect(formatRelativeTime(NOW - 59, NOW)).toBe("just now");
  });

  it("formats minutes at and after the 60s boundary", () => {
    expect(formatRelativeTime(NOW - 60, NOW)).toBe("1m ago");
    expect(formatRelativeTime(NOW - 90, NOW)).toBe("1m ago");
    expect(formatRelativeTime(NOW - 120, NOW)).toBe("2m ago");
    expect(formatRelativeTime(NOW - 3599, NOW)).toBe("59m ago");
  });

  it("formats hours at and after the 3600s boundary", () => {
    expect(formatRelativeTime(NOW - 3600, NOW)).toBe("1h ago");
    expect(formatRelativeTime(NOW - 7200, NOW)).toBe("2h ago");
    expect(formatRelativeTime(NOW - 86399, NOW)).toBe("23h ago");
  });

  it("formats days at and after the 86400s boundary", () => {
    expect(formatRelativeTime(NOW - 86400, NOW)).toBe("1d ago");
    expect(formatRelativeTime(NOW - 2591999, NOW)).toBe("29d ago");
  });

  it("formats months at and after the 30-day boundary", () => {
    expect(formatRelativeTime(NOW - 2592000, NOW)).toBe("1mo ago");
    expect(formatRelativeTime(NOW - 5184000, NOW)).toBe("2mo ago");
  });

  it("clamps future timestamps to just now instead of going negative", () => {
    expect(formatRelativeTime(NOW + 1, NOW)).toBe("just now");
    expect(formatRelativeTime(NOW + 10_000, NOW)).toBe("just now");
  });

  it("returns empty for epoch 0 (unknown timestamp sentinel)", () => {
    expect(formatRelativeTime(0, NOW)).toBe("");
  });

  it("defaults `now` to the wall clock", () => {
    const realNow = Math.floor(Date.now() / 1000);
    expect(formatRelativeTime(realNow)).toBe("just now");
    expect(formatRelativeTime(realNow - 130)).toBe("2m ago");
  });
});

describe("formatDate", () => {
  it("returns empty for epoch 0", () => {
    expect(formatDate(0)).toBe("");
  });

  it("renders a locale date-time from unix seconds", () => {
    // Contract: seconds in, Date-locale out. Compared against the same
    // conversion the component helpers used so locale differences cannot
    // make the test environment-dependent.
    expect(formatDate(1_700_000_000)).toBe(new Date(1_700_000_000 * 1000).toLocaleString());
    expect(formatDate(1_700_000_000).length).toBeGreaterThan(0);
  });
});

describe("shortHash", () => {
  it("truncates long hashes to 7 chars by default", () => {
    expect(shortHash("0123456789abcdef")).toBe("0123456");
  });

  it("honors an explicit length", () => {
    expect(shortHash("0123456789abcdef", 8)).toBe("01234567");
    expect(shortHash("0123456789abcdef", 3)).toBe("012");
  });

  it("passes through hashes shorter than or exactly the requested length", () => {
    expect(shortHash("abc")).toBe("abc");
    expect(shortHash("abcdefg")).toBe("abcdefg");
    expect(shortHash("abcdefgh", 8)).toBe("abcdefgh");
  });

  it("fails soft for empty or missing input", () => {
    expect(shortHash("")).toBe("");
    expect(shortHash(undefined)).toBe("");
    expect(shortHash(null)).toBe("");
    expect(shortHash(undefined, 8)).toBe("");
  });
});

describe("signatureLabel", () => {
  const check = (
    signature: CommitSignatureCheck["signature"],
    status: CommitSignatureCheck["status"]
  ): CommitSignatureCheck => ({ signature, status, key: "", signer: "" });

  it("never calls a commit that carries a signature unsigned because the verifier could not run", () => {
    for (const signature of ["pgp", "ssh", "x509", "other"] as const) {
      for (const status of ["none", "cannot_check"] as const) {
        const label = signatureLabel(check(signature, status));
        expect(label.text).toBe("Signed, could not verify");
        expect(label.ok).toBe(false);
      }
    }
  });

  it("calls only a commit with no signature header unsigned", () => {
    expect(signatureLabel(check("none", "none")).text).toBe("Unsigned");
  });

  it("says a check still running, or one that failed, is not a verdict", () => {
    expect(signatureLabel(undefined).text).toBe("Checking signature…");
    expect(signatureLabel(null).text).toBe("Signature not checked");
    expect(signatureLabel(check("unknown", "none")).text).toBe("Signature state unknown");
  });

  it("passes only a good signature", () => {
    expect(signatureLabel(check("ssh", "good"))).toEqual({ text: "Verified", ok: true });
    for (const status of ["bad", "good_unknown_validity", "expired", "expired_key", "revoked"] as const) {
      expect(signatureLabel(check("pgp", status)).ok).toBe(false);
    }
  });
});

describe("clock minutes", () => {
  it("round-trips HH:MM and rejects malformed or out-of-range values", async () => {
    const { formatClockMinutes, parseClockMinutes } = await import("./format");
    expect(formatClockMinutes(0)).toBe("00:00");
    expect(formatClockMinutes(23 * 60 + 59)).toBe("23:59");
    expect(parseClockMinutes("07:05")).toBe(425);
    for (const bad of ["", "24:00", "7:05", "12:60", "12:5", "ab:cd", "12:30:00"]) {
      expect(() => parseClockMinutes(bad)).toThrow("Choose valid local quiet hours.");
    }
  });
});
