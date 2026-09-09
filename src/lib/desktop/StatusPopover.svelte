<script lang="ts">
  import { slide } from "svelte/transition";
  import { Activity, Archive, ArrowDown, ArrowUp, ArrowUpRight, Check, ChevronDown, ChevronRight, CircleAlert, Command, Copy, FileDiff, FolderOpen, GitBranch, Globe, HeartPulse, History, Layers, LayoutGrid, Moon, Power, RefreshCw, Settings2, Sun, Terminal } from "@lucide/svelte";
  import type { MenuState } from "./menuState";
  import { statusDetailRows, statusInsights, statusKeyAction, statusShortcuts } from "./menuState";
  let { snapshot, error = null, pending = false, onaction }: {
    snapshot: MenuState | null; error?: string | null; pending?: boolean;
    onaction: (id: string) => void;
  } = $props();
  let expanded = $state(false);
  let choosing = $state(false);
  let copied = $state<string | null>(null);
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;
  const card = $derived(snapshot?.status);
  const hasRepo = $derived(!!snapshot?.activePath);
  const enabled = (id: string) => !pending && !!snapshot?.enabled.includes(id);
  const dark = $derived(!!snapshot?.checked.includes("theme-dark"));
  const motion = $derived(card?.reduceMotion ? 0 : 180);
  const shortcuts = $derived(snapshot ? statusShortcuts(snapshot) : []);
  const go = $derived(shortcuts.filter((item) => item.group === "go"));
  const tools = $derived(shortcuts.filter((item) => item.group === "tool"));
  const insights = $derived(card ? statusInsights(card) : []);
  const details = $derived(snapshot ? statusDetailRows(snapshot.trayDetails) : []);
  const stagedShare = $derived(card?.changed && card.changed > 0
    ? Math.min(100, Math.round(((card.staged ?? 0) / card.changed) * 100)) : null);
  const metrics = $derived([
    { label: "Changed", value: card?.changed, icon: FileDiff, action: "section:work:overview" },
    { label: "Staged", value: card?.staged, icon: Layers, action: "section:work:overview" },
    { label: "Conflicts", value: card?.conflicts, icon: CircleAlert, action: "section:work:resolve" },
    { label: "Stashes", value: card?.stashes, icon: Archive, action: "section:work:overview" },
  ]);
  const shortcutIcon = $derived<Record<string, typeof History>>({
    "section:history:graph": History, "section:insights:pulse": HeartPulse, fleet: LayoutGrid,
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

<div class="status-shell" class:reduce-motion={card?.reduceMotion} data-testid="status-popover" data-tone={card?.tone ?? "neutral"}>
  <div class="hue" aria-hidden="true"></div>
  <section class="panel" aria-label="GitPulse status">
    <header>
      <div class="mark" aria-hidden="true"><Activity size={20} strokeWidth={1.8} /></div>
      <div class="identity">
        <button class="repository" onclick={() => choosing = !choosing} disabled={(snapshot?.repositories.length ?? 0) < 2}
          aria-expanded={choosing} aria-label="Choose repository" title={snapshot?.activePath ?? "GitPulse"}>
          <span>{card?.repository ?? "GitPulse"}</span>
          {#if snapshot && snapshot.repositories.length > 1}
            <span class="count">{snapshot.repositories.length}</span>
            <ChevronDown size={13} class={choosing ? "expanded" : ""} />
          {/if}
        </button>
        <div class="branch" title={card?.branch}><GitBranch size={12} /><span>{card?.branch || "Menu bar companion"}</span></div>
      </div>
      <button class="icon-button refresh" aria-label="Refresh repository" title="Refresh · R"
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
            </span>
            {#if repo.active}<Check size={14} />{/if}
          </button>
        {/each}
      </nav>
    {/if}

    <div class="content">
      {#if !snapshot}
        <div class="empty"><div class="empty-mark"><Activity size={27} /></div><h1>{error ? "Status unavailable" : "Connecting…"}</h1>
          <p>{error || "Getting your workspace status."}</p>
          {#if error}<button class="primary" onclick={() => act("retry")}>Try again</button>{/if}
        </div>
      {:else if !hasRepo}
        <div class="empty"><div class="empty-mark"><GitBranch size={29} strokeWidth={1.5} /></div>
          <h1>Your work, at a glance</h1><p>Open a repository to see what needs attention.</p>
          <button class="primary" disabled={pending} onclick={() => act("open")}>Open repository <ArrowUpRight size={15} /></button>
          {#if enabled("clone")}
            <button class="secondary" disabled={pending} onclick={() => act("clone")}>Clone repository</button>
          {/if}
        </div>
      {:else if card}
        <div class="status-heading" aria-live="polite">
          <span class="status-dot" class:pulse={card.tone === "busy"}></span>
          <h1>{card.headline}</h1>
        </div>
        {#if stagedShare !== null}
          <div class="mix-row">
            <div class="mix" title="{stagedShare}% staged" aria-hidden="true"><span style:width="{stagedShare}%"></span></div>
            <span class="mix-caption">{card.staged ?? 0} of {card.changed} staged</span>
          </div>
        {/if}
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
              <span class="pill" class:hot={card.ahead > 0}><ArrowUp size={11} />{card.ahead} ahead</span>
              <span class="pill" class:hot={card.behind > 0}><ArrowDown size={11} />{card.behind} behind</span>
            </div>
            <span class="upstream">{card.upstream}</span>
          {:else}
            <GitBranch size={13} /><span class="upstream">{card.branch ? "No upstream data" : "Branch unavailable"}</span>
          {/if}
        </div>
        {#if insights.length}
          <div class="insights" aria-label="Workspace insights">
            {#each insights as insight}
              <button class="chip" class:warning={insight.tone === "warning"} class:busy={insight.tone === "busy"}
                disabled={!enabled(insight.id)} onclick={() => act(insight.id)}>{insight.text}</button>
            {/each}
          </div>
        {/if}
        <button class="primary" class:warning={card.tone === "warning"} disabled={!enabled(snapshot.traySummary.id)}
          onclick={() => act(snapshot.traySummary.id)}>
          {card.primaryLabel}<ArrowUpRight size={15} />
        </button>
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
        <button class="details-toggle" aria-expanded={expanded} aria-controls="status-details" onclick={() => expanded = !expanded}>
          <span><ChevronRight size={13} class={expanded ? "expanded" : ""} />Details</span>
          <span class="live" class:degraded={card.watchStatus === "degraded"} class:watching={card.watchStatus === "watching"}>
            <span></span>{card.watchStatus === "watching" ? "Live" : card.watchStatus === "degraded" ? "Not live" : "Connecting"}
          </span>
        </button>
        {#if expanded}
          <!-- Justified: the bounded details region needs focus for keyboard scrolling. -->
          <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
          <div class="details" id="status-details" tabindex="0" role="region" aria-label="Repository details"
            transition:slide={{ duration: motion }}>
            {#each details as row}
              <div>{#if row.label}<span>{row.label}</span>{/if}{row.value}</div>
            {/each}
          </div>
        {/if}
      {/if}
      {#if error && snapshot}<div class="error" role="alert"><CircleAlert size={14} /><span>{error}</span></div>{/if}
    </div>
    <footer>
      <button class="open-app" onclick={() => act("show")}>Open GitPulse <ArrowUpRight size={12} /></button>
      <div>
        <button class="icon-button" aria-label="Command palette" title="Command palette" onclick={() => act("palette")}><Command size={14} /></button>
        <button class="icon-button" aria-label="Settings" title="Settings" onclick={() => act("settings")}><Settings2 size={14} /></button>
        <button class="icon-button" aria-label="Quit GitPulse" title="Quit GitPulse" onclick={() => act("quit")}><Power size={14} /></button>
      </div>
    </footer>
  </section>
</div>

<style>
  .status-shell {
    --bg:#f6f7f9;
    --surface:#fffffff0;
    --text:#1b2027;
    --muted:#66707d;
    --line:#e3e6eb;
    --soft:#eef1f5;
    --blue:#2563eb;
    --action:#2563eb;
    --action-hover:#1e55ce;
    --green:#1f8a57;
    --amber:#b56a12;
    --hue-a:#7aa2ff;
    --hue-b:#7ddeb2;
    position:relative; padding:8px; color:var(--text); font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; font-size:12px; -webkit-font-smoothing:antialiased;
  }
  :global(.dark) .status-shell {
    --bg:#1b1f25;
    --surface:#2b3038e8;
    --text:#f3f5f8;
    --muted:#9aa3b0;
    --line:#3a404a;
    --soft:#2c323b;
    --blue:#6b93ff;
    --action:#3b74e8;
    --action-hover:#2f66d8;
    --green:#7dcaa6;
    --amber:#f3bf78;
    --hue-a:#3d5cb8;
    --hue-b:#2f7a58;
  }
  @media(prefers-color-scheme:dark) { :global(html:not(.light)) .status-shell {
    --bg:#1b1f25;
    --surface:#2b3038e8;
    --text:#f3f5f8;
    --muted:#9aa3b0;
    --line:#3a404a;
    --soft:#2c323b;
    --blue:#6b93ff;
    --action:#3b74e8;
    --action-hover:#2f66d8;
    --green:#7dcaa6;
    --amber:#f3bf78;
    --hue-a:#3d5cb8;
    --hue-b:#2f7a58;
  } }
  .hue { position:absolute; inset:8px; border-radius:18px; pointer-events:none;
    background:radial-gradient(120% 80% at 8% 0%, color-mix(in srgb, var(--hue-a) 32%, transparent), transparent 56%),
      radial-gradient(90% 70% at 100% 110%, color-mix(in srgb, var(--hue-b) 26%, transparent), transparent 52%); }
  .panel { position:relative; background:var(--bg); border:1px solid var(--line); border-radius:18px; overflow:hidden;
    box-shadow:0 12px 32px #00000016, 0 1px 3px #0000000a; }
  @supports (backdrop-filter:blur(20px)) {
    .panel { background:color-mix(in srgb, var(--bg) 78%, transparent); backdrop-filter:blur(24px) saturate(1.4); }
  }
  @media (prefers-reduced-transparency: reduce) {
    .hue { display:none; }
    .panel { background:var(--bg); backdrop-filter:none; }
  }
  button { font:inherit; cursor:pointer; border:0; color:inherit; background:none; padding:0; transition:background .16s,border-color .16s,color .16s,transform .16s,opacity .16s; }
  button:disabled { cursor:default; }
  button:focus-visible,.details:focus-visible { outline:2px solid var(--blue); outline-offset:3px; }
  .metric:active:not(:disabled),.shortcut:active:not(:disabled),.primary:active:not(:disabled),.icon-button:active:not(:disabled),.secondary:active:not(:disabled) {
    transform:translateY(1px) scale(.97);
  }
  header { display:flex; align-items:center; gap:10px; padding:16px 18px 12px; }
  .mark { width:34px; height:34px; flex-shrink:0; display:grid; place-items:center; border:1px solid var(--line); border-radius:11px; background:var(--surface); color:var(--blue); box-shadow:inset 0 1px 0 #ffffff80; }
  [data-tone="clean"] .mark { color:var(--green); }
  [data-tone="warning"] .mark { color:var(--amber); }
  [data-tone="busy"] .mark { color:var(--blue); }
  .identity { min-width:0; flex:1; }
  .repository { display:flex; align-items:center; gap:6px; max-width:100%; font-weight:650; font-size:15px; letter-spacing:-.2px; text-align:left; }
  .repository span,.branch span,.upstream { white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
  .repository:not(:disabled):hover { color:var(--blue); }
  .count { flex-shrink:0; min-width:16px; height:16px; padding:0 4px; border-radius:8px; background:var(--soft); color:var(--muted); font-size:9px; font-weight:650; display:inline-grid; place-items:center; }
  .branch { display:flex; align-items:center; gap:4px; margin-top:4px; color:var(--muted); font-size:11px; }
  .icon-button { width:28px; height:28px; display:inline-grid; place-items:center; border-radius:8px; color:var(--muted); }
  .icon-button:hover:not(:disabled) { background:var(--soft); color:var(--text); }
  .icon-button:disabled { opacity:.4; }
  .content { padding:0 18px; }
  .status-heading { display:flex; align-items:center; gap:7px; margin:0 0 8px; }
  h1 { font-size:13px; font-weight:560; margin:0; letter-spacing:-.15px; }
  .status-dot { width:6px; height:6px; border-radius:50%; background:var(--muted); flex-shrink:0; }
  [data-tone="clean"] .status-dot { background:var(--green); }
  [data-tone="warning"] .status-dot { background:var(--amber); }
  [data-tone="changed"] .status-dot,[data-tone="busy"] .status-dot { background:var(--blue); }
  .mix-row { display:flex; align-items:center; gap:8px; margin:0 0 12px; }
  .mix { flex:1; height:4px; border-radius:99px; background:var(--soft); overflow:hidden; }
  .mix span { display:block; height:100%; background:var(--blue); border-radius:inherit; }
  .mix-caption { flex-shrink:0; font-size:9px; color:var(--muted); font-variant-numeric:tabular-nums; }
  .metrics { display:grid; grid-template-columns:repeat(4,minmax(0,1fr)); gap:6px; }
  .metric { text-align:left; padding:10px 8px; border:1px solid var(--line); background:var(--surface); border-radius:11px; min-width:0; }
  .metric:hover:not(:disabled) { border-color:color-mix(in srgb, var(--blue) 55%, var(--line)); background:var(--soft); }
  .metric-label { display:flex; align-items:center; gap:3px; font-size:9px; color:var(--muted); letter-spacing:.2px; }
  .metric strong { display:block; margin-top:7px; font-size:22px; line-height:1; font-weight:560; letter-spacing:-.8px; font-variant-numeric:tabular-nums; }
  .metric.attention { color:var(--amber); border-color:color-mix(in srgb,var(--amber) 40%,var(--line)); background:color-mix(in srgb,var(--amber) 8%,var(--surface)); }
  .metric.zero strong { color:var(--muted); font-weight:500; }
  .metric.unknown strong { color:var(--muted); }
  .sync { display:flex; align-items:center; gap:8px; min-width:0; color:var(--muted); margin:12px 0; font-size:10px; }
  .sync-counts { display:flex; gap:6px; color:var(--text); font-variant-numeric:tabular-nums; }
  .pill { display:flex; align-items:center; gap:2px; padding:3px 7px; border-radius:999px; background:var(--soft); }
  .pill.hot { color:var(--blue); }
  .upstream { flex:1; }
  .insights { display:flex; flex-wrap:wrap; gap:6px; margin:0 0 12px; }
  .chip { max-width:100%; padding:5px 9px; border-radius:999px; border:1px solid var(--line); background:var(--surface); font-size:10px; color:var(--text); white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
  .chip.warning { color:var(--amber); border-color:color-mix(in srgb,var(--amber) 40%,var(--line)); background:color-mix(in srgb,var(--amber) 10%,var(--surface)); }
  .chip.busy { color:var(--blue); border-color:color-mix(in srgb,var(--blue) 35%,var(--line)); }
  .chip:hover:not(:disabled) { border-color:var(--blue); }
  .primary { display:flex; align-items:center; justify-content:center; gap:8px; width:100%; min-height:34px; color:#fff; background:var(--action); border-radius:9px; font-size:12px; font-weight:550; box-shadow:0 1px 2px #00000014; }
  .primary:hover:not(:disabled) { background:var(--action-hover); }
  .primary.warning { background:var(--amber); }
  .primary.warning:hover:not(:disabled) { filter:brightness(1.06); background:var(--amber); }
  .primary:disabled { opacity:.45; }
  .secondary { display:flex; align-items:center; justify-content:center; width:100%; min-height:32px; margin-top:8px; border:1px solid var(--line); border-radius:9px; background:var(--surface); font-weight:530; }
  .secondary:hover:not(:disabled) { border-color:var(--blue); color:var(--blue); }
  .shortcut-block { margin:12px 0 0; }
  .group-label { display:block; margin:0 0 6px; font-size:9px; font-weight:650; letter-spacing:.4px; text-transform:uppercase; color:var(--muted); }
  .shortcuts { display:grid; grid-template-columns:repeat(4,minmax(0,1fr)); gap:6px; }
  .shortcut { display:flex; flex-direction:column; align-items:center; gap:5px; min-height:52px; padding:8px 4px 7px; border:1px solid var(--line); border-radius:10px; background:var(--surface); color:var(--muted); font-size:9px; }
  .shortcut span { max-width:100%; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
  .shortcut:hover:not(:disabled) { color:var(--text); border-color:color-mix(in srgb, var(--blue) 45%, var(--line)); background:var(--soft); }
  .shortcut:disabled { opacity:.45; }
  .shortcut.copied { color:var(--green); border-color:color-mix(in srgb, var(--green) 40%, var(--line)); }
  .details-toggle { display:flex; align-items:center; justify-content:space-between; width:100%; min-height:40px; font-size:10px; color:var(--muted); }
  .details-toggle>span { display:flex; align-items:center; gap:4px; }
  .details-toggle:hover { color:var(--text); }
  .repository :global(.expanded),.details-toggle :global(.expanded) { transform:rotate(90deg); }
  .live { font-size:9px; gap:5px; }
  .live>span { width:5px; height:5px; background:var(--muted); border-radius:50%; }
  .live.watching>span { background:var(--green); animation:pulse 2s ease-in-out infinite; }
  .live.degraded { color:var(--amber); }
  .live.degraded>span { background:var(--amber); }
  .details { max-height:190px; overflow:auto; padding:0 0 12px; font-size:10px; line-height:1.5; color:var(--muted); overflow-wrap:anywhere; }
  .details>div { display:flex; gap:8px; padding:8px 0; border-top:1px solid var(--line); }
  .details span { flex:0 0 72px; color:var(--text); font-weight:550; }
  footer { display:flex; justify-content:space-between; align-items:center; border-top:1px solid color-mix(in srgb, var(--line) 80%, transparent); padding:8px 10px 8px 18px; background:color-mix(in srgb, var(--surface) 55%, transparent); }
  footer>div { display:flex; }
  .open-app { display:flex; align-items:center; gap:4px; color:var(--muted); font-size:10px; }
  .open-app:hover { color:var(--text); }
  .repositories { max-height:180px; overflow:auto; border:1px solid var(--line); background:var(--surface); margin:0 18px 12px; padding:4px; border-radius:10px; }
  .repositories button { display:flex; align-items:center; justify-content:space-between; gap:10px; width:100%; text-align:left; padding:8px; border-radius:7px; }
  .repositories span { min-width:0; display:flex; flex-direction:column; gap:2px; }
  .repositories strong { font-weight:600; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
  .repositories em { font-style:normal; font-size:10px; color:var(--muted); white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
  .repositories button:hover,.repositories .current { background:var(--soft); }
  .empty { text-align:center; padding:6px 0 22px; }
  .empty-mark { width:56px; height:56px; border-radius:16px; display:grid; place-items:center; margin:0 auto 16px; background:var(--soft); color:var(--muted); }
  .empty h1 { font-size:16px; font-weight:600; }
  .empty p { color:var(--muted); line-height:1.5; margin:9px auto 20px; max-width:230px; }
  .error { display:flex; gap:7px; color:var(--amber); font-size:11px; line-height:1.4; margin:0 0 14px; }
  .error>span { min-width:0; overflow-wrap:anywhere; }
  .error :global(svg) { flex-shrink:0; margin-top:1px; }
  :global(.rotating) { animation:spin 1.5s linear infinite; }
  .pulse { animation:pulse 1.5s ease-in-out infinite; }
  .reduce-motion :global(*) { animation:none!important; transition:none!important; }
  .reduce-motion .metric:active:not(:disabled),.reduce-motion .shortcut:active:not(:disabled),.reduce-motion .primary:active:not(:disabled) { transform:none; }
  @keyframes spin { to { transform:rotate(360deg); } }
  @keyframes pulse { 50% { opacity:.35; } }
  @media(prefers-reduced-motion:reduce) { * { animation:none!important; transition:none!important; } :global(.rotating) { animation:none!important; } }
</style>
