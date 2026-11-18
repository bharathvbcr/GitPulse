import { afterEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { clearPin, consumePin, MAX_PIN_PATH, PIN_TTL_MS, pinRequest, requestPin } from "./pinRequests";

afterEach(() => clearPin());

describe("pin requests", () => {
  it("is collected once, by the viewer of the same repository and file", () => {
    expect(requestPin("/r", "a.ts", 1000)).toBe(true);
    expect(consumePin("/other", "a.ts", 1001)).toBeNull();
    expect(consumePin("/r", "b.ts", 1001)).toBeNull();
    expect(consumePin("/r", "a.ts", 1001)).toMatchObject({ repo: "/r", path: "a.ts" });
    expect(consumePin("/r", "a.ts", 1002)).toBeNull();
  });

  it("refuses an unusable request instead of storing it", () => {
    expect(requestPin("", "a.ts")).toBe(false);
    expect(requestPin("/r", "")).toBe(false);
    expect(requestPin("/r", "x".repeat(MAX_PIN_PATH + 1))).toBe(false);
    expect(get(pinRequest)).toBeNull();
  });

  it("lets a newer request replace an older one", () => {
    requestPin("/r", "a.ts", 1000);
    requestPin("/r", "b.ts", 1001);
    expect(consumePin("/r", "a.ts", 1002)).toBeNull();
    expect(consumePin("/r", "b.ts", 1002)).not.toBeNull();
  });

  it("expires an uncollected request so it cannot pin a later, unrelated open", () => {
    requestPin("/r", "a.ts", 1000);
    expect(consumePin("/r", "a.ts", 1000 + PIN_TTL_MS + 1)).toBeNull();
    expect(get(pinRequest)).toBeNull();
  });
});
