<script lang="ts">
  import { slide } from "svelte/transition";
  import { AlertCircle, Archive, ArrowDown, ArrowUp, ArrowUpRight, Bell, Bot, Check, ChevronDown, ChevronRight, CircleAlert, Command, Copy, FileDiff, FolderOpen, GitBranch, Globe, HeartPulse, History, Layers, LayoutGrid, Moon, Power, RefreshCw, Settings2, ShieldAlert, Sun, Terminal } from "@lucide/svelte";
  import Logo from "../components/Logo.svelte";
  import LauncherIcon from "../components/LauncherIcon.svelte";
  import type { TabKind } from "../terminal/tabs";
  import { formatAge } from "../storage/format";
  import type { MenuState } from "./menuState";
  import { formatFetchAge, statusDetailRows, statusInsights, statusKeyAction, statusShortcuts } from "./menuState";
  import { createAdaptiveTimer } from "../runtime/adaptiveTimer";
  let { snapshot, error = null, pending = false, material = "opaque", onaction }: {
    snapshot: MenuState | null; error?: string | null; pending?: boolean;
    material?: "opaque" | "preview" | "native";
    onaction: (id: string) => void;
  } = $props();
  let expanded = $state(false);
  let choosing = $state(false);
  let copied = $state<string | null>(null);
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;
  let clock = $state(Date.now());
  $effect(() => {
    return createAdaptiveTimer(() => { clock = Date.now(); }, 30_000);
  });
  const card = $derived(snapshot?.status);
  const hasRepo = $derived(!!snapshot?.activePath);
  const requests = $derived(snapshot?.agentRequests ?? []);
  const requestsHidden = $derived(snapshot?.agentRequestsHidden ?? 0);
  const agentsWaitingCount = $derived(Math.max(snapshot?.agentsWaiting ?? 0, requests.length));
  const agentsRunning = $derived(snapshot?.agentsRunning ?? 0);
  const agentsInformational = $derived(snapshot?.agentsInformational ?? 0);
  const hasErrorRequest = $derived(requests.some((req) => req.urgency === "error"));
  const MAX_DATE_MS = 8.64e15;
  function safeIsoDatetime(atMs: number | null): string | undefined {
    if (typeof atMs !== "number" || !Number.isFinite(atMs) || Math.abs(atMs) > MAX_DATE_MS) {
      return undefined;
    }
    return new Date(atMs).toISOString();
  }
  function launcherKindFor(provider: string): TabKind {
    const k = provider.trim().toLowerCase();
    if (k === "claude" || k === "agy" || k === "codex" || k === "manvi" || k === "grok" || k === "shell") {
      return k;
    }
    return "shell";
  }
  const fetchCaption = $derived(
    !card || card.ahead === null || card.behind === null
      ? null
      : card.fetchedAt == null
        ? "never fetched"
        : `fetched ${formatFetchAge(card.fetchedAt, clock)}`,
  );
  const enabled = (id: string) => !pending && !!snapshot?.enabled.includes(id);
  const dark = $derived(!!snapshot?.checked.includes("theme-dark"));
  const motion = $derived(card?.reduceMotion ? 0 : 180);
  const shortcuts = $derived(snapshot ? statusShortcuts(snapshot) : []);
  const go = $derived(shortcuts.filter((item) => item.group === "go"));
  const tools = $derived(shortcuts.filter((item) => item.group === "tool"));
  const insights = $derived(card ? statusInsights(card) : []);
  const details = $derived(snapshot ? statusDetailRows(snapshot.trayDetails) : []);
  const metrics = $derived([
    { label: "Changed", value: card?.changed, icon: FileDiff, action: "section:work:overview" },
    { label: "Staged", value: card?.staged, icon: Layers, action: "section:work:overview" },
    { label: "Conflicts", value: card?.conflicts, icon: CircleAlert, action: "section:work:overview" },
  ]);
  const shortcutIcon = $derived<Record<string, typeof History>>({
    "section:history:graph": History, "section:insights:pulse": HeartPulse, fleet: LayoutGrid, agents: Bot,
    "terminal-dock": Terminal, "copy-branch": Copy, "reveal-repo": FolderOpen, "open-remote": Globe,
    "toggle-theme": dark ? Sun : Moon,
  });
  function act(id: string) {
    if (id.startsWith("copy-")) {
      copied = id;
      clearTimeout(copiedTimer);
      copiedTimer = setTimeout(() => { copied = null; }, 1400);
    }
    onaction(id);
  }
  function handleKey(event: KeyboardEvent) {
    if (
      (event.key === "ArrowDown" || event.key === "ArrowUp") &&
      !event.metaKey &&
      !event.ctrlKey &&
      !event.altKey &&
      document.activeElement instanceof HTMLElement &&
      document.activeElement.closest('[data-testid="status-agent-requests"]')
    ) {
      const rows = [
        ...document.querySelectorAll<HTMLButtonElement>(
          '[data-testid="status-agent-requests"] .request-row:not(:disabled)',
        ),
      ];
      const idx = rows.indexOf(document.activeElement as HTMLButtonElement);
      if (idx !== -1 && rows.length > 0) {
        event.preventDefault();
        const nextIdx =
          event.key === "ArrowDown"
            ? (idx + 1) % rows.length
            : (idx - 1 + rows.length) % rows.length;
        rows[nextIdx]?.focus();
        return;
      }
    }
    if (
      event.key === "Enter" &&
      event.target instanceof HTMLElement &&
      event.target.closest("button, a")
    ) {
      return;
    }
    const next = statusKeyAction(event, snapshot, { choosing, expanded });
    if (!next) return;
    event.preventDefault();
    if (next.collapse === "chooser") choosing = false;
    else if (next.collapse === "details") expanded = false;
    else if (next.dismiss) act("dismiss");
    else if (next.id && !pending) act(next.id);
  }
</script>

<svelte:window onkeydown={handleKey} />

<div class="status-shell" class:reduce-motion={card?.reduceMotion} data-testid="status-popover" data-material={material} data-tone={card?.tone ?? "neutral"}>
  <section class="panel" aria-label="GitPulse status">
    <header>
      <div class="mark" aria-hidden="true"><Logo size={24} variant="badge" /></div>
      <div class="identity">
        <button class="repository" onclick={() => choosing = !choosing} disabled={(snapshot?.repositories.length ?? 0) < 2}
          aria-expanded={choosing} aria-label="Choose repository" title={snapshot?.activePath ?? "GitPulse"}>
          <span>{card?.repository ?? "GitPulse"}</span>
          {#if snapshot && snapshot.repositories.length > 1}
            <ChevronDown size={13} class={choosing ? "expanded" : ""} />
          {/if}
        </button>
        <div class="branch" title={card?.branch}><GitBranch size={12} /><span>{card?.branch || "Menu bar companion"}</span></div>
      </div>
      <button class="gp-icon-btn" data-size="md" aria-label="Refresh repository" title="Refresh · R"
        disabled={!enabled("refresh")} onclick={() => act("refresh")}>
        <RefreshCw size={15} class={card?.tone === "busy" || pending ? "rotating" : ""} />
      </button>
    </header>

    {#if choosing && snapshot}
      <nav class="repositories" aria-label="Open repositories" transition:slide={{ duration: motion }}>
        {#each snapshot.repositories as repo (repo.path)}
          <button class:current={repo.active} disabled={pending} title={repo.path}
            onclick={() => { choosing = false; act(`activate-repo:${repo.path}`); }}>
            <span>
              <strong>{repo.label}</strong>
              <em>{repo.path}</em>
              {#if repo.busy || repo.changed != null || repo.conflicts}
                <span class="repo-badges" aria-hidden="true">
                  {#if repo.busy}<span class="badge busy">…</span>{/if}
                  {#if repo.changed}<span class="badge">{repo.changed}</span>{/if}
                  {#if repo.conflicts}<span class="badge warn">⚠{repo.conflicts}</span>{/if}
                </span>
              {/if}
            </span>
            {#if repo.active}<Check size={14} />{/if}
          </button>
        {/each}
      </nav>
    {/if}

    <div class="content">
      {#if !snapshot}
        <div class="empty"><div class="empty-mark" aria-hidden="true"><Logo size={32} variant="badge" /></div><h1>{error ? "Status unavailable" : "Connecting…"}</h1>
          <p>{error || "Getting your workspace status."}</p>
          {#if error}<button class="gp-btn w-full" data-variant="primary" data-size="md" data-testid="status-primary" onclick={() => act("retry")}>Try again</button>{/if}
        </div>
      {:else}
        {#if requests.length > 0}
          <section class="agent-requests" aria-label="Agents waiting for action" data-testid="status-agent-requests">
            <div class="requests-head">
              <span class="gp-chip" data-tone={hasErrorRequest ? "danger" : "warn"} data-kind="label">
                {#if hasErrorRequest}<AlertCircle size={11} />{:else}<ShieldAlert size={11} />{/if}
                <span>{agentsWaitingCount} waiting for action</span>
              </span>
              <button
                type="button"
                class="gp-btn"
                data-size="sm"
                data-testid="status-open-agents"
                disabled={!enabled("agents")}
                onclick={() => act("agents")}
                title="Open Agents · A"
              >Agents <ArrowUpRight size={11} /></button>
            </div>
            <div class="requests-list">
              {#each requests as req (req.id)}
                <button
                  type="button"
                  class="request-row"
                  data-testid="status-agent-request"
                  data-urgency={req.urgency}
                  data-request-id={req.id}
                  disabled={pending}
                  onclick={() => act(`focus-agent:${req.id}`)}
                  aria-label="Open {req.agentLabel}{req.repoLabel ? ` in ${req.repoLabel}` : ''}: {req.reason}"
                >
                  <span class="request-top">
                    <LauncherIcon kind={launcherKindFor(req.provider)} size={13} />
                    <span class="request-title">{req.agentLabel}</span>
                    {#if req.repoLabel}
                      <span class="request-workspace" title={req.repoPath ?? req.repoLabel}>{req.repoLabel}</span>
                    {/if}
                    {#if req.atMs && req.atMs > 0}
                      {@const datetime = safeIsoDatetime(req.atMs)}
                      <time class="request-age" {datetime}>{formatAge(req.atMs, clock)}</time>
                    {/if}
                  </span>
                  <span class="request-meta">
                    <span class="gp-chip" data-tone={req.urgency === "error" ? "danger" : "warn"} data-size="inline">
                      {#if req.urgency === "error"}<AlertCircle size={10} />{:else}<ShieldAlert size={10} />{/if}
                      <span>{req.reason}</span>
                    </span>
                    <ArrowUpRight size={11} />
                  </span>
                  {#if req.detail}
                    <p class="request-detail" title={req.detail}>{req.detail}</p>
                  {/if}
                </button>
              {/each}
            </div>
            {#if requestsHidden > 0}
              <button
                type="button"
                class="gp-btn w-full mt-1.5"
                data-variant="ghost"
                data-size="sm"
                data-testid="status-agent-requests-more"
                disabled={!enabled("agents")}
                onclick={() => act("agents")}
              >+{requestsHidden} more in Agents <ArrowUpRight size={11} /></button>
            {/if}
          </section>
        {:else if agentsWaitingCount > 0}
          <section class="agent-requests" aria-label="Agents waiting for action" data-testid="status-agent-requests">
            <div class="requests-head">
              <span class="gp-chip" data-tone="warn" data-kind="label">
                <ShieldAlert size={11} />
                <span>{agentsWaitingCount} waiting for action</span>
              </span>
              <button
                type="button"
                class="gp-btn"
                data-size="sm"
                data-testid="status-open-agents"
                disabled={!enabled("agents")}
                onclick={() => act("agents")}
                title="Open Agents · A"
              >Agents <ArrowUpRight size={11} /></button>
            </div>
            <div class="requests-list">
              <button
                type="button"
                class="request-row"
                data-testid="status-agent-fallback"
                data-urgency="blocking"
                disabled={!enabled("agents")}
                onclick={() => act("agents")}
                aria-label="Open Agents for {agentsWaitingCount} waiting {agentsWaitingCount === 1 ? 'agent' : 'agents'}"
              >
                <span class="request-top">
                  <Bot size={13} />
                  <span class="request-title">{agentsWaitingCount} {agentsWaitingCount === 1 ? "agent" : "agents"} waiting in Agents</span>
                </span>
                <span class="request-meta">
                  <span class="gp-chip" data-tone="warn" data-size="inline">
                    <ShieldAlert size={10} />
                    <span>Open Agents to review</span>
                  </span>
                  <ArrowUpRight size={11} />
                </span>
              </button>
            </div>
          </section>
        {:else if agentsRunning > 0 || agentsInformational > 0}
          <button
            type="button"
            class="agent-strip"
            data-testid="status-agent-quiet"
            data-has-notices={agentsInformational > 0}
            disabled={!enabled("agents")}
            onclick={() => act("agents")}
            title="Open Agents · A"
          >
            <span class="gp-chip" data-tone={agentsInformational > 0 ? "info" : "ok"} data-size="inline">
              {#if agentsInformational > 0}
                <Bell size={10} />
                <span>{agentsInformational} {agentsInformational === 1 ? "notice" : "notices"}</span>
              {:else}
                <Bot size={10} />
                <span>{agentsRunning} running</span>
              {/if}
            </span>
            <span class="agent-strip-text">
              {#if agentsRunning > 0 && agentsInformational > 0}
                {agentsRunning} running · none waiting
              {:else}
                No actions waiting
              {/if}
            </span>
            <ArrowUpRight size={12} />
          </button>
        {/if}
        {#if !hasRepo}
          <div class="empty"><div class="empty-mark"><GitBranch size={29} strokeWidth={1.5} /></div>
            <h1>Your work, at a glance</h1><p>Open a repository to see what needs attention.</p>
            <button class="gp-btn w-full" data-variant="primary" data-size="md" data-testid="status-primary" disabled={pending} onclick={() => act("open")}>Open repository <ArrowUpRight size={15} /></button>
            {#if enabled("clone")}
              <button class="gp-btn mt-2 w-full" data-variant="ghost" data-size="md" data-testid="status-secondary" disabled={pending} onclick={() => act("clone")}>Clone repository</button>
            {/if}
          </div>
        {:else if card}
        <div class="status-heading" aria-live="polite">
          <span class="status-dot" class:pulse={card.tone === "busy"}></span>
          <h1>{card.headline}</h1>
        </div>
        <div class="metrics" aria-label="Working tree counts">
          {#each metrics as metric}
            <button class="metric" class:attention={metric.label === "Conflicts" && !!metric.value}
              class:zero={metric.value === 0} class:unknown={metric.value == null}
              disabled={!enabled(metric.action) || !metric.value}
              aria-label={`${metric.value ?? "Unknown"} ${metric.label.toLowerCase()}`} onclick={() => act(metric.action)}>
              <span class="metric-label"><metric.icon size={12} />{metric.label}</span>
              <strong>{metric.value ?? "—"}</strong>
            </button>
          {/each}
        </div>
        <div class="sync" title="Compared with the upstream state from the last fetch">
          {#if card.ahead !== null && card.behind !== null}
            <div class="sync-counts">
              <span aria-label={`${card.ahead} ahead`}><ArrowUp size={12} />{card.ahead}</span>
              <span aria-label={`${card.behind} behind`}><ArrowDown size={12} />{card.behind}</span>
            </div>
            <span class="upstream">{card.upstream}</span><span class="caption">{fetchCaption}</span>
          {:else}
            <GitBranch size={13} /><span class="upstream">{card.branch ? "No upstream data" : "Branch unavailable"}</span>
          {/if}
        </div>
        <button class="gp-btn w-full" data-variant="primary" data-size="md" data-testid="status-primary" disabled={!enabled(snapshot.traySummary.id)}
          onclick={() => act(snapshot.traySummary.id)}>
          {card.primaryLabel}<ArrowUpRight size={15} />
        </button>
        <button class="disclosure" data-testid="status-details-toggle" aria-expanded={expanded} aria-controls="status-details" onclick={() => expanded = !expanded}>
          <span><ChevronRight size={13} class={expanded ? "expanded" : ""} />Details</span>
          <span class="live" class:degraded={card.watchStatus === "degraded"}>
            <span></span>{card.watchStatus === "watching" ? "Live" : card.watchStatus === "degraded" ? "Not live" : "Connecting"}
          </span>
        </button>
        {#if expanded}
          <!-- Justified: the bounded details region needs focus for keyboard scrolling. -->
          <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
          <div class="details" id="status-details" tabindex="0" role="region" aria-label="Repository details"
            transition:slide={{ duration: motion }}>
            <div class="detail-actions">
              <button class="gp-btn w-full justify-start" data-size="md" aria-label={`${card.stashes ?? "Unknown"} stashes`}
                disabled={!enabled("section:work:overview") || !card.stashes} onclick={() => act("section:work:overview")}>
                <Archive size={13} /><span>Stashes</span><strong class="ml-auto tabular-nums">{card.stashes ?? "—"}</strong>
              </button>
              <button class="gp-btn w-full justify-start" data-size="md" aria-label="Command palette" disabled={!enabled("palette")} onclick={() => act("palette")}>
                <Command size={13} /><span>Command palette</span>
              </button>
            </div>
            {#if insights.length}
              <div class="insights" aria-label="Workspace insights">
                {#each insights as insight}
                  <button class="gp-chip" data-tone={insight.tone === "warning" ? "warn" : insight.tone === "busy" ? "info" : "neutral"}
                    title={insight.text} disabled={!enabled(insight.id)} onclick={() => act(insight.id)}>{insight.text}</button>
                {/each}
              </div>
            {/if}
            <div class="detail-rows">
              {#each details as row}
                <div>{#if row.label}<span>{row.label}</span>{/if}{row.value}</div>
              {/each}
            </div>
            {#if go.length}
              <div class="shortcut-block">
                <span class="group-label">Go</span>
                <div class="shortcuts" aria-label="Go">
                  {#each go as item}
                    {@const Icon = shortcutIcon[item.id]}
                    <button class="shortcut" disabled={!enabled(item.id)} title={item.label} onclick={() => act(item.id)}>
                      {#if Icon}<Icon size={13} />{/if}<span>{item.label}</span>
                    </button>
                  {/each}
                </div>
              </div>
            {/if}
            {#if tools.length}
              <div class="shortcut-block">
                <span class="group-label">Tools</span>
                <div class="shortcuts" aria-label="Tools">
                  {#each tools as item}
                    {@const Icon = shortcutIcon[item.id]}
                    <button class="shortcut" class:copied={copied === item.id} disabled={!enabled(item.id)}
                      title={item.label} onclick={() => act(item.id)}>
                      {#if copied === item.id}<Check size={13} />{:else if Icon}<Icon size={13} />{/if}
                      <span>{copied === item.id ? "Copied" : item.label}</span>
                    </button>
                  {/each}
                </div>
              </div>
            {/if}
          </div>
        {/if}
      {/if}
      {#if error && snapshot}<div class="error" role="alert"><CircleAlert size={14} /><span>{error}</span></div>{/if}
      {/if}
    </div>
    <footer>
      <button class="open-app" onclick={() => act("show")}>Open GitPulse <ArrowUpRight size={12} /></button>
      <div>
        <button class="gp-icon-btn" data-size="md" aria-label="Settings" title="Settings" onclick={() => act("settings")}><Settings2 size={14} /></button>
        <button class="gp-icon-btn" data-size="md" aria-label="Quit GitPulse" title="Quit GitPulse" onclick={() => act("quit")}><Power size={14} /></button>
      </div>
    </footer>
  </section>
</div>

<style>
  /* Every colour here is a --c-pop-* token from app.css (the popover's own
     palette, solved against a black and a white wallpaper and pinned by
     StatusPopover.contrast.test.ts). The names below are this component's
     aliases for them: bare triplets for the inks, so `rgb(var(--text) / .5)`
     works, and full colours for the three fills that carry an alpha.
     This block is the opaque material. The glass arms below swap the inks to
     their glass-tuned twins. */
  .status-shell {
    --base:var(--c-pop-base);
    --card-base:var(--c-pop-card);
    --solid-muted:var(--c-pop-muted-solid);
    --glass-muted:var(--c-pop-muted-glass);
    --glass-blue:var(--c-pop-blue-glass);
    --glass-green:var(--c-pop-green-glass);
    --glass-amber:var(--c-pop-amber-glass);
    --glass-red:var(--c-pop-red-glass);
    --bg:rgb(var(--base));
    --surface:rgb(var(--card-base));
    --soft:rgb(var(--c-pop-soft));
    --line:rgb(var(--c-pop-line));
    --text:var(--c-pop-text);
    --muted:var(--solid-muted);
    --blue:var(--c-pop-blue-solid);
    --action:var(--c-pop-action);
    --action-hover:var(--c-pop-action-hover);
    --green:var(--c-pop-green-solid);
    --amber:var(--c-pop-amber-solid);
    --red:var(--c-pop-red-solid);
    /* The shared primitives (gp-btn, gp-chip, gp-icon-btn) read the app's
       tokens. Pointing those at the popover's own palette is what keeps them
       legible over a wallpaper, instead of restyling them. */
    --c-text:var(--text);
    --c-text-muted:var(--muted);
    --c-accent:var(--blue);
    --c-info:var(--blue);
    --c-ok:var(--green);
    --c-warn:var(--amber);
    --c-danger:var(--red);
    --c-surface:var(--card-base);
    --c-surface-hover:var(--c-pop-soft);
    --c-on-accent:var(--c-pop-on-action);
    --gp-edge:var(--line);
    --brand-gradient:linear-gradient(rgb(var(--action)), rgb(var(--action)));
    --shadow-glow:0 1px 2px rgb(var(--c-pop-shadow) / .07);
    /* app.css's preflight puts line-height:1.5 on html; this window was laid out
       at the browser default, and the native window is sized to its content. */
    line-height:normal;
    padding:8px; color:rgb(var(--text)); font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; font-size:var(--text-xs); -webkit-font-smoothing:antialiased;
  }
  .panel { max-height:620px; display:flex; flex-direction:column; background:var(--bg); border:1px solid var(--line); border-radius: var(--gp-r-xl); overflow:hidden; box-shadow:0 6px 20px rgb(var(--c-pop-shadow) / .07),0 1px 3px rgb(var(--c-pop-shadow) / .03); }
  /* ── Liquid blur glass ──
     The native window server supplies desktop blur via NSVisualEffectView.
     On top of that the page paints the same hue field and glass fills as the
     main app so the popover reads as part of the same surface language.
     The browser fixture ("preview") stands in for that material; the one thing
     it cannot have, the desktop blur, is a CSS filter that lives in
     harness/StatusPreview.svelte, because only the fixture needs it and only
     the float rule in app.css may blur in a component.

     Both materials therefore share one ladder and one pair of appearance
     arms. They used to be two copies, and the copy drifted exactly the way
     copies do: the fixture carried the base ladder but neither arm, so in
     light mode it drew the dark tuning — a .05 sheen where the material has
     .7, a hard black .22 shade where the material has a .07 navy, thin .4
     cards where the material has .58, and the dark hue blobs at twice the
     gain. A fixture tuned differently from the surface it represents
     validates something that never ships, so the two are now the same
     declaration rather than two that have to be kept in step. */
  .status-shell[data-material="native"],
  .status-shell[data-material="preview"] {
    --liquid-ease:var(--gp-ease-effect);
    /* Hue blobs — same four colours as .gp-shell in app.css, per theme. */
    --hue-a:var(--c-pop-hue-a); --hue-b:var(--c-pop-hue-b); --hue-c:var(--c-pop-hue-c); --hue-d:var(--c-pop-hue-d);
    --hue-gain:1;
    /* Glass surface ladder — mirrors --mac-glass-fill / --mac-fill-surface. */
    --glass-edge:rgb(var(--c-pop-white) / .14);
    --glass-sheen-top:rgb(var(--c-pop-white) / .05);
    --glass-shade-bottom:rgb(var(--c-pop-shade) / .22);
    --glass-tint:rgb(var(--c-pop-tint) / .07);
    /* 0.8 is a floor, not a taste choice. This panel floats over an arbitrary
       desktop, so every foreground token has to clear 4.5:1 against BOTH a
       pure-white and a pure-black wallpaper, and panel opacity is the only
       thing decoupling the text from it. Solved in
       StatusPopover.contrast.test.ts against the palette in app.css: the
       minimum passing alpha is .787 light and .795 dark. At .55 the accents
       land at 2.1-2.5:1 — and no single colour can clear 4.5:1 at both
       extremes once the panel is that thin, so darkening the tokens instead
       is not available. The glass still reads: 20% of the desktop shows
       through, under the hue field and the sheen and tint passes below. */
    --bg:rgb(var(--base) / .8);
    --surface:rgb(var(--card-base) / .4);
    --soft:rgb(var(--card-base) / .28);
    --line:var(--glass-edge);
    --muted:var(--glass-muted); --blue:var(--glass-blue); --green:var(--glass-green); --amber:var(--glass-amber); --red:var(--glass-red);
    /* Hue field — scaled for the popover's small footprint. */
    background-image:
      radial-gradient(90% 80% at 2% 0%, rgb(var(--hue-a) / calc(.5 * var(--hue-gain))), rgb(var(--hue-a) / 0) 100%),
      radial-gradient(80% 76% at 98% 6%, rgb(var(--hue-c) / calc(.42 * var(--hue-gain))), rgb(var(--hue-c) / 0) 100%),
      radial-gradient(82% 78% at 88% 100%, rgb(var(--hue-b) / calc(.38 * var(--hue-gain))), rgb(var(--hue-b) / 0) 100%),
      radial-gradient(72% 68% at 6% 96%, rgb(var(--hue-d) / calc(.3 * var(--hue-gain))), rgb(var(--hue-d) / 0) 100%);
  }
  /* Native only: the window server owns the frame, so the page must not add a
     gutter around it. The fixture keeps the shell's 8px so the simulated
     backdrop stays visible around the panel. */
  .status-shell[data-material="native"] { padding:0; }
  :global(:where(.dark)) .status-shell[data-material="native"],
  :global(:where(.dark)) .status-shell[data-material="preview"],
  :global(:where(html:not(.light))) .status-shell[data-material="native"],
  :global(:where(html:not(.light))) .status-shell[data-material="preview"] {
    --glass-sheen-top:rgb(var(--c-pop-white) / .05);
    --glass-shade-bottom:rgb(var(--c-pop-shade) / .22);
    --glass-tint:rgb(var(--c-pop-tint) / .07);
  }
  .status-shell[data-material="native"]:not(:global(:where(.dark)) *, :global(:where(html:not(.light))) *),
  .status-shell[data-material="preview"]:not(:global(:where(.dark)) *, :global(:where(html:not(.light))) *) {
    --hue-gain:.5;
    /* Same floor as the dark arm above; light solves to .787. */
    --bg:rgb(var(--base) / .8);
    --surface:rgb(var(--card-base) / .58);
    --soft:rgb(var(--card-base) / .36);
    --glass-edge:rgb(var(--card-base) / .24);
    --glass-sheen-top:rgb(var(--c-pop-white) / .7);
    --glass-shade-bottom:rgb(var(--c-pop-shade) / .07);
    --glass-tint:rgb(var(--c-pop-tint) / .05);
    --line:var(--glass-edge);
  }
  .status-shell[data-material="native"] .panel,
  .status-shell[data-material="preview"] .panel {
    border-color:var(--glass-edge);
    /* Two-pass glass treatment matching gp-glass: specular sheen + hue tint. */
    background-image:
      linear-gradient(155deg, var(--glass-sheen-top), transparent 46%),
      linear-gradient(200deg, var(--glass-tint), transparent 72%);
    box-shadow:
      inset 0 1px 0 var(--glass-sheen-top),
      inset 0 -1px 0 var(--glass-shade-bottom);
  }
  /* Inner surfaces thin so they read as glass cards inside the panel. */
  .status-shell[data-material="native"] :is(.metric, .repositories, .gp-chip:not([data-tone]), .mark, .gp-btn:not([data-variant]), .request-row, .agent-strip),
  .status-shell[data-material="preview"] :is(.metric, .repositories, .gp-chip:not([data-tone]), .mark, .gp-btn:not([data-variant]), .request-row, .agent-strip) {
    border-color:var(--glass-edge);
    background:var(--surface);
  }
  /* Liquid-ease transitions on interactive elements. */
  @media (prefers-reduced-motion: no-preference) {
    .status-shell[data-material="native"]:not(.reduce-motion) :is(button, .metric, .shortcut, .request-row, .agent-strip),
    .status-shell[data-material="preview"]:not(.reduce-motion) :is(button, .metric, .shortcut, .request-row, .agent-strip) {
      transition-property:color, background-color, border-color, transform;
      transition-duration:var(--gp-dur-fast);
      transition-timing-function:var(--liquid-ease);
    }
    .status-shell[data-material="native"]:not(.reduce-motion) :is(.metric, .shortcut, .request-row):active:not(:disabled),
    .status-shell[data-material="preview"]:not(.reduce-motion) :is(.metric, .shortcut, .request-row):active:not(:disabled) {
      transform:scale(0.97);
    }
  }
  /* With no CSS filter there is no desktop blur to stand in for, so the fixture
     drops the glass rather than show a translucent panel over an unblurred
     page — the one thing the shipping material never is. This is the same
     opaque surface the accessibility preferences below fall back to. The
     native material needs no such gate: its blur does not come from CSS. */
  @supports not ((backdrop-filter:blur(1px)) or (-webkit-backdrop-filter:blur(1px))) {
    .status-shell[data-material="preview"] { --bg:rgb(var(--base)); --surface:rgb(var(--card-base)); --muted:var(--solid-muted); background-image:none; }
    .status-shell[data-material="preview"] .panel { background-image:none; box-shadow:none; }
  }
  @media (prefers-reduced-transparency: reduce), (prefers-contrast: more), (forced-colors: active) {
    .status-shell[data-material] { --bg:rgb(var(--base)); --surface:rgb(var(--card-base)); --muted:var(--solid-muted); background-image:none; }
    .status-shell[data-material] .panel { -webkit-backdrop-filter:none; backdrop-filter:none; background-image:none; box-shadow:none; }
    .status-shell[data-material] :is(.metric, .repositories, .gp-chip:not([data-tone]), .mark, .gp-btn:not([data-variant]), .request-row, .agent-strip) { background:var(--surface); border-color:var(--line); }
  }
  /* WKWebView does not implement prefers-reduced-transparency, so the query above
     never fires for the macOS Reduce Transparency setting. The native bridge
     (lib/ui/transparencyPreference.ts) stamps this attribute instead. These are
     the query block's first two rules; keep them identical. Its third rule
     (inner cards: background var(--surface), border var(--line)) has nothing to
     add here: the glass rule above already gives those cards var(--surface) and
     var(--glass-edge), and --line is var(--glass-edge). */
  :global(html[data-transparency="reduced"]) .status-shell[data-material] { --bg:rgb(var(--base)); --surface:rgb(var(--card-base)); --muted:var(--solid-muted); background-image:none; }
  :global(html[data-transparency="reduced"]) .status-shell[data-material] .panel { -webkit-backdrop-filter:none; backdrop-filter:none; background-image:none; box-shadow:none; }
  /* The shared button resets come from app.css's preflight; only the pointer
     is the component's to say, and never over a gp- primitive that already did. */
  button:not(.gp-btn, .gp-icon-btn, .gp-chip) { font:inherit; cursor:pointer; color:inherit; }
  button:disabled:not(.gp-btn, .gp-icon-btn) { cursor:default; }
  .gp-chip:disabled { opacity:.45; cursor:default; }
  /* Tinted chips on .panel (.requests-head, .insights) sit directly on translucent
     --bg rather than a --surface card; backing them with --surface keeps every
     tone >= 4.5:1 across glass wallpapers and opaque hover states. */
  .status-shell .gp-chip[data-tone="ok"] { background-color:color-mix(in srgb,rgb(var(--c-ok)) 10%,var(--surface)); }
  .status-shell .gp-chip[data-tone="info"] { background-color:color-mix(in srgb,rgb(var(--c-info)) 10%,var(--surface)); }
  .status-shell .gp-chip[data-tone="warn"] { background-color:color-mix(in srgb,rgb(var(--c-warn)) 10%,var(--surface)); }
  .status-shell .gp-chip[data-tone="danger"] { background-color:color-mix(in srgb,rgb(var(--c-danger)) 10%,var(--surface)); }
  button:focus-visible,.details:focus-visible { outline:2px solid rgb(var(--blue)); outline-offset:3px; box-shadow:none; }
  /* The primary fill's hover, from the popover's own solved token rather than
     the shared brightness filter, which would walk the white label off 4.5:1. */
  .status-shell :global(.gp-btn[data-variant="primary"]:hover:not(:disabled)) { background-image:linear-gradient(rgb(var(--action-hover)), rgb(var(--action-hover))); filter:none; }
  .status-shell :global(.gp-btn:not([data-variant]):hover:not(:disabled)) { background:var(--soft); }
  header { flex-shrink:0; display:flex; align-items:center; gap:10px; padding:20px 20px 17px; }
  .mark { width:34px; height:34px; flex-shrink:0; display:grid; place-items:center; border:1px solid var(--line); border-radius: var(--gp-r-md); background:var(--surface); }
  .identity { min-width:0; flex:1; }
  .repository { display:flex; align-items:center; gap:6px; max-width:100%; font-weight:600; font-size:var(--text-base); letter-spacing:-.2px; text-align:left; }
  .repository span,.branch span,.upstream { white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
  .repository:not(:disabled):hover { color:rgb(var(--blue)); }
  .branch { display:flex; align-items:center; gap:4px; margin-top:5px; color:rgb(var(--muted)); font-size:var(--text-meta); }
  .content { flex:1 1 auto; min-height:0; overflow-y:auto; padding:0 20px; }
  .agent-requests { margin:0 0 14px; }
  .requests-head { display:flex; align-items:center; justify-content:space-between; gap:8px; margin-bottom:8px; }
  .requests-list { display:flex; flex-direction:column; gap:6px; max-height:152px; overflow-y:auto; padding:2px; }
  .request-row { display:flex; flex-direction:column; gap:5px; width:100%; text-align:left; padding:9px 10px; border:1px solid var(--line); background:var(--surface); border-radius:var(--gp-r-md); transition:background var(--gp-dur-fast),border-color var(--gp-dur-fast); }
  .status-shell .request-row[data-urgency="blocking"] { border-color:color-mix(in srgb,rgb(var(--amber)) 40%,var(--line)); background:color-mix(in srgb,rgb(var(--amber)) 7%,var(--surface)); }
  .status-shell .request-row[data-urgency="error"] { border-color:color-mix(in srgb,rgb(var(--red)) 40%,var(--line)); background:color-mix(in srgb,rgb(var(--red)) 7%,var(--surface)); }
  .status-shell .request-row:hover:not(:disabled) { border-color:rgb(var(--blue)); background:var(--soft); }
  .request-top { display:flex; align-items:center; gap:6px; min-width:0; width:100%; }
  .request-title { font-weight:600; font-size:var(--text-xs); color:rgb(var(--text)); white-space:nowrap; overflow:hidden; text-overflow:ellipsis; min-width:0; }
  .request-workspace { font-size:var(--text-micro); color:rgb(var(--muted)); white-space:nowrap; overflow:hidden; text-overflow:ellipsis; min-width:0; flex:1 1 auto; }
  .request-age { flex-shrink:0; margin-left:auto; font-size:var(--text-micro); color:rgb(var(--muted)); font-variant-numeric:tabular-nums; }
  .request-meta { display:flex; align-items:center; justify-content:space-between; gap:8px; width:100%; color:rgb(var(--muted)); }
  .request-detail { margin:0; font-size:var(--text-micro); color:rgb(var(--muted)); line-height:1.35; display:-webkit-box; -webkit-line-clamp:2; line-clamp:2; -webkit-box-orient:vertical; overflow:hidden; overflow-wrap:anywhere; }
  .agent-strip { display:flex; align-items:center; gap:8px; width:100%; padding:8px 10px; margin:0 0 14px; border:1px solid var(--line); background:var(--surface); border-radius:var(--gp-r-md); text-align:left; font-size:var(--text-micro); color:rgb(var(--muted)); transition:background var(--gp-dur-fast),border-color var(--gp-dur-fast); }
  .status-shell .agent-strip:hover:not(:disabled) { border-color:rgb(var(--blue)); background:var(--soft); color:rgb(var(--text)); }
  .agent-strip-text { flex:1; min-width:0; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
  .status-heading { display:flex; align-items:center; gap:7px; margin:0 0 16px; }
  h1 { font-size:var(--text-xs); font-weight:500; margin:0; letter-spacing:-.1px; }
  .status-dot { width:6px; height:6px; border-radius:50%; background:rgb(var(--muted)); flex-shrink:0; }
  [data-tone="clean"] .status-dot { background:rgb(var(--green)); }
  [data-tone="warning"] .status-dot { background:rgb(var(--amber)); }
  [data-tone="changed"] .status-dot,[data-tone="busy"] .status-dot { background:rgb(var(--blue)); }
  .metrics { display:grid; grid-template-columns:repeat(3,minmax(0,1fr)); gap:8px; }
  .metric { text-align:left; padding:12px 10px; border:1px solid var(--line); background:var(--surface); border-radius: var(--gp-r-md); min-width:0; transition:background var(--gp-dur-fast),border-color var(--gp-dur-fast); }
  .metric:hover:not(:disabled) { border-color:rgb(var(--blue)); background:var(--soft); }
  .metric-label { display:flex; align-items:center; gap:4px; font-size:var(--text-micro); color:rgb(var(--muted)); }
  .metric strong { display:block; margin-top:8px; font-size:var(--text-display); line-height:1; font-weight:600; letter-spacing:-.6px; font-variant-numeric:tabular-nums; }
  .metric.attention { color:rgb(var(--amber)); border-color:color-mix(in srgb,rgb(var(--amber)) 40%,var(--line)); background:color-mix(in srgb,rgb(var(--amber)) 7%,var(--surface)); }
  .sync { display:flex; align-items:center; gap:8px; min-width:0; color:rgb(var(--muted)); margin:14px 0 18px; font-size:var(--text-micro); }
  .sync-counts { display:flex; gap:9px; color:rgb(var(--text)); font-variant-numeric:tabular-nums; }
  .sync-counts span { display:flex; align-items:center; gap:2px; }
  .upstream { flex:1; }
  .caption { font-size:var(--text-micro); }
  .disclosure { display:flex; align-items:center; justify-content:space-between; width:100%; min-height:43px; font-size:var(--text-micro); color:rgb(var(--muted)); }
  .disclosure>span { display:flex; align-items:center; gap:4px; }
  .disclosure:hover { color:rgb(var(--text)); }
  .disclosure :global(.expanded) { transform:rotate(90deg); }
  .live { font-size:var(--text-micro); }
  .live>span { width:4px; height:4px; background:rgb(var(--muted)); border-radius:50%; }
  .live.degraded { color:rgb(var(--amber)); }
  .live.degraded>span { background:rgb(var(--amber)); }
  .details { max-height:190px; overflow:auto; padding:0 0 14px; margin-bottom:2px; font-size:var(--text-micro); line-height:1.5; color:rgb(var(--muted)); overflow-wrap:anywhere; }
  .detail-rows { display:flex; flex-direction:column; gap:2px; }
  .detail-rows>div { display:flex; gap:8px; padding:7px 0; }
  .detail-rows span { flex:0 0 62px; font-weight:500; }
  .detail-actions { display:grid; grid-template-columns:repeat(2,minmax(0,1fr)); gap:6px; padding-bottom:12px; }
  .shortcut-block { padding:10px 0 0; }
  .group-label { display:block; margin-bottom:6px; font-size:var(--text-micro); font-weight:500; }
  .shortcuts { display:grid; grid-template-columns:repeat(2,minmax(0,1fr)); gap:4px; padding-bottom:10px; }
  .shortcut { display:flex; align-items:center; gap:6px; padding:8px; border-radius: var(--gp-r-sm); text-align:left; font-size:var(--text-micro); }
  .shortcut:hover:not(:disabled) { color:rgb(var(--text)); background:var(--soft); }
  .shortcut:disabled { opacity:.45; }
  .shortcut.copied { color:rgb(var(--green)); }
  .insights { display:flex; flex-wrap:wrap; gap:6px; padding-bottom:12px; }
  .repository :global(.expanded) { transform:rotate(180deg); }
  footer>div { display:flex; }
  .metric.unknown strong { color:rgb(var(--muted)); }
  footer { flex-shrink:0; display:flex; justify-content:space-between; align-items:center; padding:8px 14px 8px 20px; }
  .open-app { display:flex; align-items:center; gap:4px; color:rgb(var(--muted)); font-size:var(--text-micro); }
  .open-app:hover { color:rgb(var(--text)); }
  .repositories { max-height:180px; overflow:auto; border:1px solid var(--line); background:var(--surface); margin:0 20px 15px; padding:4px; border-radius: var(--gp-r-md); }
  .repositories button { display:flex; align-items:center; justify-content:space-between; gap:10px; width:100%; text-align:left; padding:8px; border-radius: var(--gp-r-sm); }
  .repositories span { min-width:0; display:flex; flex-direction:column; gap:3px; }
  .repositories strong,.repositories em { white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
  .repositories strong { font-weight:500; }
  .repositories em { font-size:var(--text-micro); font-style:normal; color:rgb(var(--muted)); }
  .repositories button:hover,.repositories .current { background:var(--soft); }
  .repo-badges { display:flex; flex-direction:row; gap:4px; margin-top:2px; }
  .repo-badges .badge { font-size:var(--text-micro); font-variant-numeric:tabular-nums; color:rgb(var(--muted)); background:var(--soft); border-radius: var(--gp-r-xs); padding:1px 5px; }
  .repo-badges .badge.warn { color:rgb(var(--amber)); }
  .repo-badges .badge.busy { letter-spacing:.5px; }
  .empty { text-align:center; padding:6px 0 22px; }
  .empty-mark { width:56px; height:56px; border-radius: var(--gp-r-lg); display:grid; place-items:center; margin:0 auto 16px; background:var(--soft); color:rgb(var(--muted)); }
  .empty h1 { font-size:var(--text-base); font-weight:600; }
  .empty p { color:rgb(var(--muted)); line-height:1.5; margin:9px auto 20px; max-width:230px; }
  .error { display:flex; gap:7px; color:rgb(var(--amber)); font-size:var(--text-meta); line-height:1.4; margin:0 0 14px; }
  .error>span { min-width:0; overflow-wrap:anywhere; }
  .error :global(svg) { flex-shrink:0; margin-top:1px; }
  :global(.rotating) { animation:spin calc(var(--gp-dur-emphasis) * 3) linear infinite; }
  .pulse { animation:pulse calc(var(--gp-dur-emphasis) * 3) var(--gp-ease-effect) infinite; }
  .reduce-motion :global(*) { animation:none!important; transition:none!important; }
  @keyframes spin { to { transform:rotate(360deg); } }
  @keyframes pulse { 50% { opacity:.35; } }
  @media(prefers-reduced-motion:reduce) { * { animation:none!important; transition:none!important; } :global(.rotating) { animation:none!important; } }
</style>
