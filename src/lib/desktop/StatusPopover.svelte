<script lang="ts">
  import { Activity, ArrowDown, ArrowUp, ArrowUpRight, Check, ChevronDown, ChevronRight, CircleAlert, FileDiff, GitBranch, Layers, Power, RefreshCw, Settings2 } from "@lucide/svelte";
  import type { MenuState } from "./menuState";
  let { snapshot, error = null, pending = false, onaction }: {
    snapshot: MenuState | null; error?: string | null; pending?: boolean;
    onaction: (id: string) => void;
  } = $props();
  let expanded = $state(false);
  let choosing = $state(false);
  const card = $derived(snapshot?.status);
  const hasRepo = $derived(!!snapshot?.activePath);
  const enabled = (id: string) => !pending && !!snapshot?.enabled.includes(id);
  const metrics = $derived([
    { label: "Changed", value: card?.changed, icon: FileDiff, action: "section:work:overview" },
    { label: "Staged", value: card?.staged, icon: Layers, action: "section:work:overview" },
    { label: "Conflicts", value: card?.conflicts, icon: CircleAlert, action: "section:work:resolve" },
  ]);
</script>

<div class="status-shell" class:reduce-motion={card?.reduceMotion} data-testid="status-popover" data-tone={card?.tone ?? "neutral"}>
  <section class="panel" aria-label="GitPulse status">
    <header>
      <div class="mark" aria-hidden="true"><Activity size={21} strokeWidth={1.8} /></div>
      <div class="identity">
        <button class="repository" onclick={() => choosing = !choosing} disabled={(snapshot?.repositories.length ?? 0) < 2}
          aria-expanded={choosing} aria-label="Choose repository" title={snapshot?.activePath ?? "GitPulse"}>
          <span>{card?.repository ?? "GitPulse"}</span>
          {#if snapshot && snapshot.repositories.length > 1}<ChevronDown size={13} />{/if}
        </button>
        <div class="branch" title={card?.branch}><GitBranch size={12} /><span>{card?.branch || "Menu bar companion"}</span></div>
      </div>
      <button class="icon-button refresh" aria-label="Refresh repository" title="Refresh repository"
        disabled={!enabled("refresh")} onclick={() => onaction("refresh")}>
        <RefreshCw size={15} class={card?.tone === "busy" || pending ? "rotating" : ""} />
      </button>
    </header>

    {#if choosing && snapshot}
      <nav class="repositories" aria-label="Open repositories">
        {#each snapshot.repositories as repo (repo.path)}
          <button class:current={repo.active} disabled={pending} title={repo.path}
            onclick={() => { choosing = false; onaction(`activate-repo:${repo.path}`); }}>
            <span>{repo.label}</span>{#if repo.active}<Check size={14} />{/if}
          </button>
        {/each}
      </nav>
    {/if}

    <div class="content">
      {#if !snapshot}
        <div class="empty"><div class="empty-mark"><Activity size={27} /></div><h1>{error ? "Status unavailable" : "Connecting…"}</h1>
          <p>{error || "Getting your workspace status."}</p>
          {#if error}<button class="primary" onclick={() => onaction("retry")}>Try again</button>{/if}
        </div>
      {:else if !hasRepo}
        <div class="empty"><div class="empty-mark"><GitBranch size={29} strokeWidth={1.5} /></div>
          <h1>Your work, at a glance</h1><p>Open a repository to see what needs attention.</p>
          <button class="primary" disabled={pending} onclick={() => onaction("open")}>Open repository <ArrowUpRight size={15} /></button>
        </div>
      {:else if card}
        <div class="status-heading" aria-live="polite">
          <span class="status-dot" class:pulse={card.tone === "busy"}></span>
          <h1>{card.headline}</h1>
        </div>
        <div class="metrics" aria-label="Working tree counts">
          {#each metrics as metric}
            <button class="metric" class:attention={metric.label === "Conflicts" && !!metric.value}
              disabled={!enabled(metric.action) || !metric.value}
              aria-label={`${metric.value ?? "Unknown"} ${metric.label.toLowerCase()}`} onclick={() => onaction(metric.action)}>
              <span class="metric-label"><metric.icon size={12} />{metric.label}</span>
              <strong>{metric.value ?? "—"}</strong>
            </button>
          {/each}
        </div>
        <div class="sync" title="Compared with the upstream state from the last fetch">
          {#if card.ahead !== null && card.behind !== null}
            <div class="sync-counts"><span><ArrowUp size={12} />{card.ahead}</span><span><ArrowDown size={12} />{card.behind}</span></div>
            <span class="upstream">{card.upstream}</span><span class="caption">last fetch</span>
          {:else}
            <GitBranch size={13} /><span class="upstream">{card.branch ? "No upstream data" : "Branch unavailable"}</span>
          {/if}
        </div>
        <button class="primary" disabled={!enabled(snapshot.traySummary.id)} onclick={() => onaction(snapshot.traySummary.id)}>
          {card.primaryLabel}<ArrowUpRight size={15} />
        </button>
        <button class="details-toggle" aria-expanded={expanded} aria-controls="status-details" onclick={() => expanded = !expanded}>
          <span><ChevronRight size={13} class={expanded ? "expanded" : ""} />Details</span>
          <span class="live" class:degraded={card.watchStatus === "degraded"}>
            <span></span>{card.watchStatus === "watching" ? "Live" : card.watchStatus === "degraded" ? "Not live" : "Connecting"}
          </span>
        </button>
        {#if expanded}
          <!-- Justified: the bounded details region needs focus for keyboard scrolling. -->
          <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
          <div class="details" id="status-details" tabindex="0" role="region" aria-label="Repository details">
            {#each snapshot.trayDetails as detail}<div>{detail}</div>{/each}
          </div>
        {/if}
      {/if}
      {#if error && snapshot}<div class="error" role="alert"><CircleAlert size={14} /><span>{error}</span></div>{/if}
    </div>
    <footer>
      <button class="open-app" onclick={() => onaction("show")}>Open GitPulse <ArrowUpRight size={12} /></button>
      <div><button class="icon-button" aria-label="Settings" title="Settings" onclick={() => onaction("settings")}><Settings2 size={14} /></button>
      <button class="icon-button" aria-label="Quit GitPulse" title="Quit GitPulse" onclick={() => onaction("quit")}><Power size={14} /></button></div>
    </footer>
  </section>
</div>

<style>
  .status-shell { 
    --bg:#fafbfc;
    --surface:#fff;
    --text:#20242c;
    --muted:#69727f;
    --line:#e5e8ed;
    --soft:#f0f2f5;
    --blue:#2563eb;
    --action:#2563eb;
    --action-hover:#1e55ce;
    --green:#258354;
    --amber:#ac6719; padding:8px; color:var(--text); font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; font-size:12px; -webkit-font-smoothing:antialiased; 
  }
  :global(.dark) .status-shell { 
    --bg:#202329;
    --surface:#292d34;
    --text:#f0f2f5;
    --muted:#a1a9b6;
    --line:#383d46;
    --soft:#2b3038;
    --blue:#558aff;
    --action:#326ade;
    --action-hover:#285fcf;
    --green:#76c59d;
    --amber:#f1bb70; 
  }
  @media(prefers-color-scheme:dark) { :global(html:not(.light)) .status-shell { 
    --bg:#202329;
    --surface:#292d34;
    --text:#f0f2f5;
    --muted:#a1a9b6;
    --line:#383d46;
    --soft:#2b3038;
    --blue:#558aff;
    --action:#326ade;
    --action-hover:#285fcf;
    --green:#76c59d;
    --amber:#f1bb70; 
  } }
  .panel { background:var(--bg); border:1px solid var(--line); border-radius:18px; overflow:hidden; box-shadow:0 6px 20px #00000012,0 1px 3px #00000008; }
  button { font:inherit; cursor:pointer; border:0; color:inherit; background:none; padding:0; }
  button:disabled { cursor:default; }
  button:focus-visible,.details:focus-visible { outline:2px solid var(--blue); outline-offset:3px; }
  header { display:flex; align-items:center; gap:10px; padding:20px 20px 17px; }
  .mark { width:34px; height:34px; flex-shrink:0; display:grid; place-items:center; border:1px solid var(--line); border-radius:10px; background:var(--surface); }
  .identity { min-width:0; flex:1; }
  .repository { display:flex; align-items:center; gap:6px; max-width:100%; font-weight:650; font-size:15px; letter-spacing:-.2px; text-align:left; }
  .repository span,.branch span,.upstream { white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
  .repository:not(:disabled):hover { color:var(--blue); }
  .branch { display:flex; align-items:center; gap:4px; margin-top:5px; color:var(--muted); font-size:11px; }
  .icon-button { width:28px; height:28px; display:inline-grid; place-items:center; border-radius:7px; color:var(--muted); }
  .icon-button:hover:not(:disabled) { background:var(--soft); color:var(--text); }
  .icon-button:disabled { opacity:.4; }
  .content { padding:0 20px; }
  .status-heading { display:flex; align-items:center; gap:7px; margin:0 0 16px; }
  h1 { font-size:12px; font-weight:500; margin:0; letter-spacing:-.1px; }
  .status-dot { width:6px; height:6px; border-radius:50%; background:var(--muted); flex-shrink:0; }
  [data-tone="clean"] .status-dot { background:var(--green); }
  [data-tone="warning"] .status-dot { background:var(--amber); }
  [data-tone="changed"] .status-dot,[data-tone="busy"] .status-dot { background:var(--blue); }
  .metrics { display:grid; grid-template-columns:repeat(3,minmax(0,1fr)); gap:8px; }
  .metric { text-align:left; padding:12px 10px; border:1px solid var(--line); background:var(--surface); border-radius:10px; min-width:0; transition:background .14s,border-color .14s; }
  .metric:hover:not(:disabled) { border-color:var(--blue); background:var(--soft); }
  .metric-label { display:flex; align-items:center; gap:4px; font-size:10px; color:var(--muted); }
  .metric strong { display:block; margin-top:8px; font-size:27px; line-height:1; font-weight:550; letter-spacing:-1px; font-variant-numeric:tabular-nums; }
  .metric.attention { color:var(--amber); border-color:color-mix(in srgb,var(--amber) 40%,var(--line)); background:color-mix(in srgb,var(--amber) 7%,var(--surface)); }
  .sync { display:flex; align-items:center; gap:8px; min-width:0; color:var(--muted); margin:14px 0 18px; font-size:10px; }
  .sync-counts { display:flex; gap:9px; color:var(--text); font-variant-numeric:tabular-nums; }
  .sync-counts span { display:flex; align-items:center; gap:2px; }
  .upstream { flex:1; }
  .caption { font-size:9px; }
  .primary { display:flex; align-items:center; justify-content:center; gap:8px; width:100%; min-height:34px; color:#fff; background:var(--action); border-radius:8px; font-size:12px; font-weight:550; box-shadow:0 1px 2px #00000012; }
  .primary:hover:not(:disabled) { background:var(--action-hover); }
  .primary:disabled { opacity:.45; }
  .details-toggle { display:flex; align-items:center; justify-content:space-between; width:100%; min-height:43px; font-size:10px; color:var(--muted); }
  .details-toggle>span { display:flex; align-items:center; gap:4px; }
  .details-toggle:hover { color:var(--text); }
  .details-toggle :global(.expanded) { transform:rotate(90deg); }
  .live { font-size:9px; }
  .live>span { width:4px; height:4px; background:var(--muted); border-radius:50%; }
  .live.degraded { color:var(--amber); }
  .live.degraded>span { background:var(--amber); }
  .details { max-height:190px; overflow:auto; padding:0 0 14px; margin-bottom:2px; font-size:10px; line-height:1.5; color:var(--muted); overflow-wrap:anywhere; }
  .details>div { padding:7px 0; border-top:1px solid var(--line); }
  footer { display:flex; justify-content:space-between; align-items:center; border-top:1px solid var(--line); padding:8px 14px 8px 20px; }
  .open-app { display:flex; align-items:center; gap:4px; color:var(--muted); font-size:10px; }
  .open-app:hover { color:var(--text); }
  .repositories { max-height:180px; overflow:auto; border:1px solid var(--line); background:var(--surface); margin:0 20px 15px; padding:4px; border-radius:9px; }
  .repositories button { display:flex; align-items:center; justify-content:space-between; gap:10px; width:100%; text-align:left; padding:8px; border-radius:5px; }
  .repositories span { white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
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
  @keyframes spin { to { transform:rotate(360deg); } }
  @keyframes pulse { 50% { opacity:.35; } }
  @media(prefers-reduced-motion:reduce) { * { animation:none!important; transition:none!important; } :global(.rotating) { animation:none!important; } }
</style>
