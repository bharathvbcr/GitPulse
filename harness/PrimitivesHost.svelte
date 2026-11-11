<script lang="ts">
  import Toggle from "../src/lib/components/Toggle.svelte";
  import Checkbox from "../src/lib/components/Checkbox.svelte";
  import Slider from "../src/lib/components/Slider.svelte";
  import Dialog from "../src/lib/components/Dialog.svelte";

  /**
   * The shared primitives with nothing around them, so each check drives one
   * production component through real DOM events. State a check needs to read
   * is written into `data-*` attributes and the counters below, never into
   * the primitives themselves.
   */
  let on = $state(false);
  let changes = $state(0);
  let labelled = $state(false);
  let disabledChanges = $state(0);
  let ticked = $state(false);
  let tickChanges = $state(0);
  let confirmed = $state(false);
  let mixed = $state(true);
  let refused = $state(false);
  let disabledTicks = $state(0);
  let level = $state(20);
  let inputs = $state(0);
  let md = $state(false);
  let sheet = $state(false);
  let locked = $state(false);
  let closes = $state(0);
</script>

<div class="flex w-96 flex-col gap-4 p-6">
  <div data-testid="toggles" class="flex flex-col gap-2">
    <Toggle ariaLabel="Bare switch" checked={on} onchange={(next) => { on = next; changes += 1; }} />
    <Toggle label="Labelled switch" checked={labelled} onchange={(next) => (labelled = next)} />
    <Toggle ariaLabel="Disabled switch" disabled onchange={() => (disabledChanges += 1)} />
  </div>
  <div data-testid="checkboxes" class="flex flex-col gap-2">
    <Checkbox ariaLabel="Bare box" checked={ticked} onchange={(next) => { ticked = next; tickChanges += 1; }} />
    <Checkbox label="Labelled box" description="Says what ticking commits to." checked={confirmed} onchange={(next) => (confirmed = next)} />
    <Checkbox ariaLabel="Mixed box" indeterminate={mixed} checked={false} onchange={() => (mixed = false)} />
    <Checkbox ariaLabel="Refusing box" checked={refused} onchange={() => {}} />
    <Checkbox ariaLabel="Disabled box" disabled onchange={() => (disabledTicks += 1)} />
  </div>
  <div data-testid="checkbox-state" data-ticked={ticked} data-tick-changes={tickChanges} data-confirmed={confirmed} data-mixed={mixed} data-disabled-ticks={disabledTicks}></div>
  <div data-testid="slider">
    <Slider label="Level" min={0} max={100} step={5} bind:value={level} format={(n) => `${n}%`} oninput={() => (inputs += 1)} />
  </div>
  <div class="flex gap-2">
    <button type="button" id="open-md" class="gp-btn" onclick={() => (md = true)}>Open md</button>
    <button type="button" id="open-sheet" class="gp-btn" onclick={() => (sheet = true)}>Open sheet</button>
    <button type="button" id="open-locked" class="gp-btn" onclick={() => (locked = true)}>Open locked</button>
  </div>
  <div data-testid="state" data-changes={changes} data-on={on} data-labelled={labelled} data-disabled-changes={disabledChanges} data-level={level} data-inputs={inputs} data-md={md} data-sheet={sheet} data-locked={locked} data-closes={closes}></div>
</div>

<Dialog bind:open={md} title="Delete branch?" onclose={() => (closes += 1)}>
  <input class="gp-field w-full" id="md-field" aria-label="Branch name" />
  {#snippet footer()}
    <button type="button" class="gp-btn" id="md-cancel" onclick={() => (md = false)}>Cancel</button>
    <button type="button" class="gp-btn" id="md-delete" onclick={() => (md = false)}>Delete</button>
  {/snippet}
</Dialog>

<Dialog bind:open={sheet} size="sheet" title="Sheet" onclose={() => (closes += 1)}>
  <input class="gp-field w-full" id="sheet-field" aria-label="Filter" />
  {#snippet footer()}
    <button type="button" class="gp-btn" id="sheet-done" onclick={() => (sheet = false)}>Done</button>
  {/snippet}
</Dialog>

<Dialog bind:open={locked} dismissable={false} title="Working" onclose={() => (closes += 1)}>
  <button type="button" class="gp-btn" id="locked-inner">Inner</button>
  {#snippet footer()}
    <button type="button" class="gp-btn" id="locked-finish" onclick={() => (locked = false)}>Finish</button>
  {/snippet}
</Dialog>
