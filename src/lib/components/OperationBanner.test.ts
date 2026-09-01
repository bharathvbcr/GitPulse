import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import OperationBanner, { armedLabel } from "./OperationBanner.svelte";
import { IDLE_OPERATION, type OperationState, type RepoOperation } from "../repos/operation";

function op(extra: Partial<RepoOperation> = {}): RepoOperation {
  return {
    kind: "Merge",
    current_step: null,
    total_steps: null,
    head_ref: "main",
    incoming_ref: null,
    conflicted_paths: [],
    conflicted_total: 0,
    available: ["abort"],
    ...extra,
  };
}

function state(extra: Partial<OperationState> = {}): OperationState {
  return { operation: null, probeFailed: false, ...extra };
}

describe("OperationBanner", () => {
  it("renders nothing for an idle repository", () => {
    const { body } = render(OperationBanner, {
      props: { operationState: IDLE_OPERATION },
    });
    // SSR still emits its hydration comment markers; what matters is that no
    // element or text reaches the page.
    expect(body.replace(/<!--.*?-->/g, "").trim()).toBe("");
  });

  it("states the operation and the next step", () => {
    const { body } = render(OperationBanner, {
      props: {
        operationState: state({
          operation: op({ conflicted_total: 2, available: ["abort"] }),
        }),
      },
    });
    expect(body).toContain("Merge in progress");
    expect(body).toContain("2 files");
    expect(body).toContain("Abort merge");
  });

  it("offers continue only once the backend allows it, and leads with it", () => {
    const blocked = render(OperationBanner, {
      props: {
        operationState: state({ operation: op({ conflicted_total: 1, available: ["abort"] }) }),
      },
    }).body;
    expect(blocked).not.toContain("Commit the merge");

    const clear = render(OperationBanner, {
      props: {
        operationState: state({
          operation: op({ conflicted_total: 0, available: ["abort", "continue"] }),
        }),
      },
    }).body;
    expect(clear).toContain("Commit the merge");
    // The forward action must render before the destructive one.
    expect(clear.indexOf("Commit the merge")).toBeLessThan(clear.indexOf("Abort merge"));
  });

  it("never renders a failed probe as a clean repository", () => {
    const { body } = render(OperationBanner, {
      props: { operationState: state({ probeFailed: true }) },
    });
    expect(body).toContain("Repository state unknown");
    // And it offers no action buttons: acting on state we could not read is
    // exactly what must not happen.
    expect(body).not.toContain("Abort");
    expect(body).not.toContain("gp-btn-primary");
  });

  it("surfaces backend warnings instead of swallowing them", () => {
    const { body } = render(OperationBanner, {
      props: {
        operationState: state({
          operation: op({ warnings: ["cannot list conflicted files: boom"] }),
        }),
      },
    });
    expect(body).toContain("cannot list conflicted files: boom");
  });

  it("puts the consequence of each action on the control itself", () => {
    const { body } = render(OperationBanner, {
      props: {
        operationState: state({
          operation: op({ kind: "Rebase", available: ["abort", "skip"] }),
        }),
      },
    });
    // Tooltips carry the warning before the click, not after.
    expect(body).toContain("discarded");
    expect(body).toContain("will not appear");
  });

  it("labels the bisect escape the way git spells it", () => {
    const { body } = render(OperationBanner, {
      props: {
        operationState: state({ operation: op({ kind: "Bisect", available: ["abort"] }) }),
      },
    });
    expect(body).toContain("End bisect");
    expect(body).not.toContain("Abort bisect");
  });

  it("shows what is being applied when the backend knows", () => {
    const { body } = render(OperationBanner, {
      props: {
        operationState: state({
          operation: op({ kind: "CherryPick", incoming_ref: "abc1234 add parser" }),
        }),
      },
    });
    expect(body).toContain("abc1234 add parser");
  });

  it("escapes hostile ref text rather than emitting it as markup", () => {
    const { body } = render(OperationBanner, {
      props: {
        operationState: state({
          operation: op({ incoming_ref: "<script>alert(1)</script>" }),
        }),
      },
    });
    // Svelte escapes the opening angle bracket, which is what prevents the
    // tag from ever being parsed as markup; `>` is left as a literal.
    expect(body).not.toContain("<script>alert(1)</script>");
    expect(body).toContain("&lt;script>alert(1)&lt;/script>");
  });
});

describe("armedLabel", () => {
  it("marks the armed state so the second click is visibly a confirmation", () => {
    expect(armedLabel("Abort merge", false)).toBe("Abort merge");
    expect(armedLabel("Abort merge", true)).toBe("Confirm: Abort merge");
  });
});
