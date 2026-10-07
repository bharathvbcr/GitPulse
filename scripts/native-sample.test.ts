import { describe, expect, it } from "vitest";
import { parsePs, sampleTree, summarize } from "./native-sample.mjs";

// Shaped from a real `ps -A -o pid=,ppid=,rss=,%cpu=,comm=` on 2026-10-07: the
// installed app had agent CLIs in its terminal tabs as children.
const PS = `
    1     0  12000   0.1 /sbin/launchd
21385     1 171344   1.5 /Applications/GitPulse.app/Contents/MacOS/gitpulse
21394 21385   6896   0.0 /Users/me/.local/bin/manvi
32846 21385 612512   0.1 /Users/me/.local/bin/claude
40000 32846  20000   0.2 /Applications/GitPulse.app/Contents/MacOS/gitpulse-mcp
40001 21385   3000   0.0 /Applications/GitPulse.app/Contents/MacOS/gitpulsed
50000     1 400000   3.0 /System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent
`;

describe("native sampler", () => {
  it("keeps the app, its bundled helpers and the processes it hosts apart", () => {
    const tree = sampleTree(parsePs(PS), 21385);
    expect(tree).toEqual({
      app: { rssKib: 171344, cpu: 1.5 },
      // Bundled binaries count as GitPulse's own, wherever they sit in the tree.
      helpers: { processes: 2, rssKib: 23000, cpu: 0.2 },
      // An agent CLI in a terminal tab is the user's work, not GitPulse's.
      hosted: { processes: 2, rssKib: 619408, cpu: 0.1 },
    });
  });

  it("reports an exited app as no sample, not as zero memory", () => {
    expect(sampleTree(parsePs(PS), 99999)).toBeNull();
  });

  it("states the memory trend as a slope and names what it did not measure", () => {
    const hour = 3_600_000;
    const at = (h: number, mib: number) => ({ t: h * hour, app: { rssKib: mib * 1024, cpu: 1 } });
    const summary = summarize([at(0, 100), at(1, 110), at(2, 120), at(3, 130)]);
    expect(summary).toMatchObject({ samples: 4, hours: 3, appRssMib: { first: 100, last: 130, min: 100, max: 130 }, appCpuMean: 1 });
    expect(summary.appRssSlopeMibPerHour).toBeCloseTo(10);
    expect(summary.notMeasured).toMatch(/WKWebView/);
    expect(summarize([])).toEqual({ samples: 0 });
  });
});
