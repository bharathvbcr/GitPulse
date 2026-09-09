import type { MenuState } from "../src/lib/desktop/menuState";
export function statusFixture(kind: string): MenuState {
  const snapshot: MenuState = {
    enabled: ["open", "refresh", "section:work:overview", "section:work:resolve", "section:history:graph"],
    checked: [], labels: [], activePath: "/Projects/GitPulse", showStatusIcon: true,
    repositories: [{ path: "/Projects/GitPulse", label: "GitPulse", active: true }, { path: "/Projects/ScholarLM", label: "ScholarLM", active: false }],
    trayDetail: "GitPulse · feature/status-popover", traySummary: { id: "section:work:overview", text: "12 changed · 4 staged" },
    trayDetails: ["Repository: /Projects/GitPulse", "Branch: feature/status-popover", "12 changed · 4 staged · 0 conflicts", "2 listed stashes", "3 ahead · 1 behind origin/main (last fetch)", "Live updates"],
    status: { repository: "GitPulse", branch: "feature/status-popover", changed: 12, staged: 4, conflicts: 0, ahead: 3, behind: 1,
      upstream: "origin/main", headline: "Ready to review", tone: "changed", primaryLabel: "Review changes", watchStatus: "watching", reduceMotion: false },
  };
  if (kind === "conflicts") { snapshot.status.conflicts = 2; snapshot.status.headline = "Conflicts need your attention"; snapshot.status.tone = "warning"; snapshot.status.primaryLabel = "Resolve conflicts"; snapshot.traySummary = { id: "section:work:resolve", text: "Resolve 2 conflicts" }; snapshot.trayDetails[2] = "12 changed · 4 staged · 2 conflicts"; }
  if (kind === "clean") { Object.assign(snapshot.status, { changed: 0, staged: 0, headline: "All changes committed", tone: "clean", primaryLabel: "View history", ahead: 0, behind: 0 }); snapshot.traySummary = { id: "section:history:graph", text: "Clean" }; snapshot.trayDetails[2] = "0 changed · 0 staged · 0 conflicts"; snapshot.trayDetails[4] = "0 ahead · 0 behind origin/main (last fetch)"; }
  if (kind === "loading") { Object.assign(snapshot.status, { changed: null, staged: null, conflicts: null, ahead: null, behind: null, upstream: null, headline: "Loading…", tone: "busy", primaryLabel: "Try again" }); snapshot.traySummary = { id: "refresh", text: "Loading…" }; snapshot.enabled = ["open"]; snapshot.trayDetails = ["Repository: /Projects/GitPulse", "Loading status…"]; }
  if (kind === "unavailable") { Object.assign(snapshot.status, { changed: null, staged: null, conflicts: null, ahead: null, behind: null, upstream: null, headline: "Status unavailable", tone: "warning", primaryLabel: "Try again", watchStatus: "degraded" }); snapshot.traySummary = { id: "refresh", text: "Status unavailable" }; snapshot.enabled = ["open", "refresh"]; snapshot.trayDetails = ["Repository: /Projects/GitPulse", "Status unavailable"]; }
  if (kind === "empty") { snapshot.activePath = null; snapshot.repositories = []; snapshot.status.branch = ""; snapshot.enabled = ["open"]; }
  if (kind === "long names") { snapshot.status.repository = "A very long repository name that exceeds the width"; snapshot.status.branch = "feature/long-branch-name-with-many-segments-and-details"; snapshot.status.upstream = "upstream/feature/very-long-tracking-branch"; }
  return snapshot;
}
