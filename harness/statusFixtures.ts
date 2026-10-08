import type { MenuState } from "../src/lib/desktop/menuState";

const NAV = [
  "open", "clone", "refresh", "palette", "fleet", "terminal-dock", "toggle-theme",
  "copy-branch", "copy-repo-path", "copy-commit", "reveal-repo", "open-remote",
  "section:work:overview", "section:work:resolve", "section:history:graph", "section:insights:pulse",
];

export function statusFixture(kind: string): MenuState {
  const snapshot: MenuState = {
    enabled: [...NAV],
    checked: [], labels: [], activePath: "/Projects/GitPulse", showStatusIcon: true,
    hideDockWhenClosed: true, trayTitle: null,
    repositories: [
      { path: "/Projects/GitPulse", label: "GitPulse", active: true, changed: 12, conflicts: 0, busy: false },
      { path: "/Projects/ScholarLM", label: "ScholarLM", active: false, changed: 3, conflicts: 1, busy: true },
    ],
    trayDetail: "GitPulse · feature/status-popover", traySummary: { id: "section:work:overview", text: "12 changed · 4 staged" },
    trayDetails: ["Repository: /Projects/GitPulse", "Branch: feature/status-popover", "12 changed · 4 staged · 0 conflicts", "2 listed stashes", "3 ahead · 1 behind origin/main (fetched 4 min ago)", "Live updates"],
    status: { repository: "GitPulse", branch: "feature/status-popover", changed: 12, staged: 4, conflicts: 0, ahead: 3, behind: 1,
      upstream: "origin/main", headline: "Ready to review", tone: "changed", primaryLabel: "Review changes", watchStatus: "watching", reduceMotion: false,
      stashes: 2, operation: null, activity: null, elsewhere: 0, fetchedAt: Date.now() - 4 * 60_000 },
  };
  if (kind === "conflicts") { snapshot.status.conflicts = 2; snapshot.status.headline = "Conflicts need your attention"; snapshot.status.tone = "warning"; snapshot.status.primaryLabel = "Resolve conflicts"; snapshot.traySummary = { id: "section:work:resolve", text: "Resolve 2 conflicts" }; snapshot.trayDetails[2] = "12 changed · 4 staged · 2 conflicts"; snapshot.repositories[0].conflicts = 2; }
  if (kind === "clean") { Object.assign(snapshot.status, { changed: 0, staged: 0, stashes: 0, headline: "All changes committed", tone: "clean", primaryLabel: "View history", ahead: 0, behind: 0 }); snapshot.traySummary = { id: "section:history:graph", text: "Clean" }; snapshot.trayDetails[2] = "0 changed · 0 staged · 0 conflicts"; snapshot.trayDetails[4] = "0 ahead · 0 behind origin/main (fetched 4 min ago)"; snapshot.repositories[0].changed = 0; snapshot.repositories[0].conflicts = 0; }
  if (kind === "loading") { Object.assign(snapshot.status, { changed: null, staged: null, conflicts: null, ahead: null, behind: null, upstream: null, stashes: null, headline: "Loading…", tone: "busy", primaryLabel: "Try again", fetchedAt: null }); snapshot.traySummary = { id: "refresh", text: "Loading…" }; snapshot.enabled = ["open", "clone", "palette"]; snapshot.trayDetails = ["Repository: /Projects/GitPulse", "Loading status…"]; }
  if (kind === "unavailable") { Object.assign(snapshot.status, { changed: null, staged: null, conflicts: null, ahead: null, behind: null, upstream: null, stashes: null, headline: "Status unavailable", tone: "warning", primaryLabel: "Try again", watchStatus: "degraded", fetchedAt: null }); snapshot.traySummary = { id: "refresh", text: "Status unavailable" }; snapshot.enabled = ["open", "refresh", "palette"]; snapshot.trayDetails = ["Repository: /Projects/GitPulse", "Status unavailable"]; }
  if (kind === "empty") { snapshot.activePath = null; snapshot.repositories = []; snapshot.status.branch = ""; snapshot.enabled = ["open", "clone", "palette"]; snapshot.status.fetchedAt = null; }
  if (kind === "long names") { snapshot.status.repository = "A very long repository name that exceeds the width"; snapshot.status.branch = "feature/long-branch-name-with-many-segments-and-details"; snapshot.status.upstream = "upstream/feature/very-long-tracking-branch"; }
  if (kind === "busy") { Object.assign(snapshot.status, { activity: "Fetching…", tone: "busy", headline: "Fetching…", elsewhere: 1, primaryLabel: "Review changes" }); snapshot.traySummary = { id: "section:work:overview", text: "Fetching… · 1 running elsewhere" }; }
  if (kind === "operation") { Object.assign(snapshot.status, { operation: "Merge in progress — on main", tone: "warning", headline: "Merge in progress — on main", primaryLabel: "Review changes" }); }
  if (kind === "never fetched") { snapshot.status.fetchedAt = null; snapshot.trayDetails[4] = "3 ahead · 1 behind origin/main (never fetched)"; }
  return snapshot;
}
