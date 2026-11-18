import { describe, expect, it } from "vitest";
import { headerAgentPlane } from "./headerPlane";
import { terminalSessions } from "../terminal/sessionRegistry";
import { sessionActivity } from "../terminal/sessionActivity";

describe("headerAgentPlane OSC title churn isolation", () => {
  it("does not re-emit to RepoTabBar/ViewBar subscribers when only OSC session titles change", () => {
    const slot = terminalSessions.reserve({
      key: "tab-churn",
      repoPath: "/work/repo-alpha",
      label: "Claude",
      launcher: "claude",
      title: "Initial task",
      status: "running",
      sessionId: "term-churn-1",
      close: async () => {},
    });

    let emissions = 0;
    const stop = headerAgentPlane.subscribe(() => {
      emissions += 1;
    });
    try {
      expect(emissions).toBe(1);

      // Simulate 10 Hz OSC title retitles via both sessionRegistry and sessionActivity:
      for (let i = 0; i < 12; i += 1) {
        slot.retitle(`Working step ${i}`);
        sessionActivity.title("term-churn-1", `Working step ${i}`);
      }
      expect(emissions).toBe(1);

      // Attention announcements ARE header-relevant and MUST re-emit immediately:
      sessionActivity.announce({
        session: "term-churn-1",
        channel: "hook",
        event: "permission_prompt",
        detail: "Bash: cargo test",
      });
      expect(emissions).toBe(2);
    } finally {
      stop();
      sessionActivity.forget("term-churn-1");
      slot.release();
    }
  });
});
