<script lang="ts">
  import { interfaceStore } from "../stores/interfaceStore";
  import { authorColor, authorIdentity } from "./authorIdentity";
  import { avatarUrlFor, githubAvatarsVersion } from "./githubAvatars";

  /**
   * One author disc: the hue-coded initials are always the base layer, and a
   * GitHub profile picture, when one is known and the setting is on, is
   * overlaid on top. A picture that fails to load is dropped and the initials
   * stay, so an author is never left without a mark.
   */
  interface Props {
    name: string | null | undefined;
    email: string | null | undefined;
    /** Size, text and layout classes of the call site. */
    class?: string;
    title?: string;
    ariaHidden?: boolean;
  }
  let { name, email, class: className = "", title, ariaHidden = false }: Props = $props();

  const identity = $derived(authorIdentity(name, email));
  // The store reads make the URL re-derive when lookups land or the setting flips.
  const url = $derived.by(() => {
    void $githubAvatarsVersion;
    void $interfaceStore.showGithubAvatars;
    return avatarUrlFor(email);
  });
  let failedUrl = $state<string | null>(null);
</script>

<span
  class="relative {className}"
  style="background-color: {authorColor(identity.hue)}"
  {title}
  aria-hidden={ariaHidden ? "true" : undefined}
  data-author-avatar
>
  {identity.initials}
  {#if url !== null && failedUrl !== url}
    <img
      class="absolute inset-0 h-full w-full rounded-full object-cover"
      src={url}
      referrerpolicy="no-referrer"
      decoding="async"
      alt=""
      onerror={() => (failedUrl = url)}
    />
  {/if}
</span>
