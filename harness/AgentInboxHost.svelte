<script lang="ts">
  import { untrack } from "svelte";
  import AgentInbox from "../src/lib/components/AgentInbox.svelte";
  import type { AgentInboxThread } from "../src/lib/agents/inboxTypes";
  import type { MobileKickoffDirective } from "../src/lib/agents/collaboration";

  interface Props {
    initialRepoPath?: string;
    focusThreadKey?: string | null;
    focusSeq?: number;
    onopenTask?: (taskId: string) => Promise<void>;
    onkickoff?: (directive: MobileKickoffDirective, thread: AgentInboxThread) => Promise<void> | void;
  }

  let {
    initialRepoPath,
    focusThreadKey = null,
    focusSeq = 0,
    onopenTask,
    onkickoff,
  }: Props = $props();

  let currentRepoPath = $state<string | undefined>(untrack(() => initialRepoPath));
  let currentFocusKey = $state<string | null>(untrack(() => focusThreadKey));
  let currentFocusSeq = $state<number>(untrack(() => focusSeq));

  export function setRepoPath(next: string | undefined): void {
    currentRepoPath = next;
  }

  export function setFocus(threadKey: string | null, seq: number): void {
    currentFocusKey = threadKey;
    currentFocusSeq = seq;
  }
</script>

<AgentInbox
  repoPath={currentRepoPath}
  focusThreadKey={currentFocusKey}
  focusSeq={currentFocusSeq}
  {onopenTask}
  {onkickoff}
/>
