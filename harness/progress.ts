/**
 * How far a harness page got, told to the runner while it runs.
 *
 * A page killed at its deadline sends no verdict, so the runner could say only
 * "did not finish" and the last file Vite served, which names a module the page
 * imported early, not where the time went. The runner (scripts/browser-regressions.mjs)
 * listens on `/__gp_progress/<id>`; each beat says how many checks have been
 * recorded and the last one's name, and the runner puts the newest beat in the
 * timeout message and the first and last beat's times in a passing page's
 * timing line.
 *
 * Beats are at most one per `BEAT_MS` (the newest wins, flushed by a timer), so a
 * 500-check page does not make 500 requests. Nothing here can fail a page: a
 * missing report parameter (a page opened by hand) or a refused request is
 * ignored.
 */
const BEAT_MS = 1000;

let pending: { count: number; name: string } | null = null;
let timer: ReturnType<typeof setTimeout> | undefined;

function progressUrl(): string | null {
  const report = new URL(location.href).searchParams.get("report");
  if (!report?.startsWith("/__gp_result/")) return null;
  return report.replace("/__gp_result/", "/__gp_progress/");
}

function flush(): void {
  timer = undefined;
  const beat = pending;
  const url = progressUrl();
  pending = null;
  if (!beat || !url) return;
  try {
    void fetch(url, { method: "POST", body: JSON.stringify(beat), keepalive: true }).catch(() => {});
  } catch {
    // Progress is advisory; the verdict is what counts.
  }
}

/** Records that the page has now recorded `count` checks, the last named `name`. */
export function reportProgress(count: number, name: string): void {
  pending = { count, name: name.slice(0, 200) };
  if (timer === undefined) timer = setTimeout(flush, BEAT_MS);
}
