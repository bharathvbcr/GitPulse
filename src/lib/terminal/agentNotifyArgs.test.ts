/**
 * What the renderer contributes to an agent CLI's argv: the prompt, and
 * nothing else.
 *
 * The notification flags used to be added here too, which is why this file
 * has the name it has. They moved to the backend
 * (`terminal_command::notify_flags`, tested in Rust) because a copy here only
 * ever reached the tabs this renderer launches; a task attempt, which never
 * passes through it, ran with none. What these tests protect now is that the
 * copy does not come back, and the prompt shapes the backend's flags are put
 * in front of.
 */
import { describe, expect, it } from "vitest";
import * as launchRequests from "./launchRequests";
import { agentPromptArgs } from "./launchRequests";

describe("agent launch arguments from the renderer", () => {
  it("carries no notification flag table of its own", () => {
    const exported = Object.keys(launchRequests);
    expect(exported).not.toContain("AGENT_NOTIFY_ARGS");
    expect(exported).not.toContain("agentNotifyArgs");
  });

  it("gives Claude Code its prompt behind the separator, as one literal argument", () => {
    // `claude -- <prompt>` makes everything after `--` positional; the
    // backend puts its flags in front of this, never behind.
    expect(agentPromptArgs("claude", "Fix the failing test")).toEqual(["--", "Fix the failing test"]);
  });

  it("leaves Antigravity's own prompt flag last", () => {
    expect(agentPromptArgs("agy", "Look at this")).toEqual(["--prompt-interactive", "Look at this"]);
  });

  it("a promptless agent launch contributes nothing", () => {
    expect(agentPromptArgs("codex", undefined)).toBeNull();
  });
});
