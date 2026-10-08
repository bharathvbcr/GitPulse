<script lang="ts">
  import PullRequestDetail from "../src/lib/components/github/PullRequestDetail.svelte";
  import PullRequestCreate from "../src/lib/components/github/PullRequestCreate.svelte";

  let {
    onChanged,
    onCreated,
  }: { onChanged: () => void; onCreated: (message: string) => void } = $props();

  /** #7 open, #8 draft, #9 closed — each detail loads its own fixture. */
  const NUMBERS = [7, 8, 9];
</script>

<main class="flex flex-col gap-3 p-3 bg-background text-textPrimary max-w-3xl">
  <section data-create="branch">
    <PullRequestCreate repoPath="/fixture/GitPulse" slug="acme/gitpulse" headBranch="feat/search" defaultBase="main" {onCreated} onCancel={() => {}} />
  </section>
  <section data-create="detached">
    <PullRequestCreate repoPath="/fixture/GitPulse" slug="acme/gitpulse" headBranch={null} defaultBase="main" {onCreated} onCancel={() => {}} />
  </section>
  {#each NUMBERS as number (number)}
    <PullRequestDetail repoPath="/fixture/GitPulse" {number} slug="acme/gitpulse" {onChanged} />
  {/each}
</main>
