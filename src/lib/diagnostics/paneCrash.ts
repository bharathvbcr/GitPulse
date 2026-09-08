import { formatDiagnosticFailure, type DiagnosticsStore } from "./diagnostics";

export interface PaneContext {
  view: string;
  section: string | null;
  repo: string | null;
  file: string | null;
}

/** One visible Code subpane owns these nonreactive request breadcrumbs. */
export function createPaneDetails() {
  let owner: symbol | null = null;
  let current: { repo: string | null; section: string; text: string } | null = null;
  return {
    register(section: string) {
      const token = Symbol(section);
      owner = token;
      current = null;
      return {
        update(repo: string | null, subview: string, requests: Record<string, number>) {
          if (owner !== token) return;
          current = { repo, section, text: bounded(JSON.stringify({
            subview: bounded(subview, 48),
            requests: Object.fromEntries(Object.entries(requests).slice(0, 8)
              .filter(([, value]) => Number.isSafeInteger(value) && value >= 0)
              .map(([key, value]) => [bounded(key, 32), value])),
          }), 300) };
        },
        dispose() { if (owner === token) { owner = null; current = null; } },
      };
    },
    read(context: PaneContext): string | null {
      return context.view === "code" && current && current.repo === context.repo && current.section === context.section ? current.text : null;
    },
  };
}

export const paneDetails = createPaneDetails();

/** Redact before clipping so truncation cannot split a credential detector. */
function bounded(detail: unknown, cap: number): string {
  const text = formatDiagnosticFailure(detail);
  return text.length <= cap ? text : `${text.slice(0, cap - 14)}… [truncated]`;
}

function contextText(context: PaneContext): string {
  return JSON.stringify({
    view: bounded(context.view, 48),
    section: bounded(context.section ?? "none", 48),
    repo: bounded(context.repo ?? "none", 180),
    file: bounded(context.file ?? "none", 180),
  });
}

/**
 * Nonreactive, bounded navigation breadcrumbs. Normal navigation never adds
 * an error/warning. A failure snapshots the evidence before deferring its
 * store write out of Svelte's render pass; later navigation cannot relabel it.
 */
export function createPaneCrashReporter(
  sink: Pick<DiagnosticsStore, "error">,
  readContext: () => PaneContext,
  defer: (work: () => void) => void = (work) => { setTimeout(work, 0); },
) {
  const history: string[] = [];
  let previous = "";

  function observe(context: PaneContext): void {
    const text = contextText(context);
    if (text === previous) return;
    previous = text;
    history.push(`${new Date().toISOString()} ${text}`);
    if (history.length > 8) history.shift();
  }

  function report(pane: string, error: unknown): void {
    let context = "context unavailable";
    let detail: string | null = null;
    try {
      const snapshot = readContext();
      context = contextText(snapshot);
      detail = paneDetails.read(snapshot);
    } catch {
      // Preserve the original error even if its context cannot be read.
    }
    let stack = "stack unavailable";
    try {
      if (error !== null && typeof error === "object" && "stack" in error && typeof error.stack === "string") {
        stack = error.stack;
      }
    } catch {
      // Error.stack can itself be a throwing getter or Proxy trap.
    }
    stack = formatDiagnosticFailure(stack);
    // The top identifies the runtime throw; the tail often contains the
    // application component. Keep both instead of retaining only internals.
    if (stack.length > 650) stack = `${stack.slice(0, 300)}\n… [truncated]\n${stack.slice(-330)}`;
    const message = [
      `Pane: ${bounded(pane, 60)}`,
      `Context: ${bounded(context, 470)}`,
      ...(detail ? [`Requests: ${detail}`] : []),
      `Error: ${bounded(error, 260)}`,
      `Stack: ${stack}`,
      `Recent navigation (${history.length} retained):`,
      ...history.slice(-2).map((entry) => bounded(entry, 210)),
    ].join("\n");
    const snapshot = bounded(message, 2000);
    defer(() => sink.error("pane-crash", snapshot));
  }

  return { observe, report };
}
