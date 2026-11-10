<script lang="ts" module>
  /** The static plate index.html paints before any script runs. */
  export const SPLASH_ID = "gp-boot";

  /** The slice of an element the hand-off touches, so a test needs no DOM. */
  export interface SplashElement {
    style: { transitionDuration: string };
    setAttribute(name: string, value: string): void;
    remove(): void;
  }

  /**
   * Hands the plate off to the shell: an opacity fade of `fadeMs`, then the
   * element is removed. At 0 ms (reduced motion) it is a cut. Returns false when
   * there is no plate (already removed by the safety timer, or a harness page).
   * The removal does not wait on a `transitionend` event, which never fires when
   * the transition is skipped.
   */
  export function handOffSplash(
    plate: SplashElement | null,
    fadeMs: number,
    schedule: (callback: () => void, ms: number) => unknown = (callback, ms) => setTimeout(callback, ms),
  ): boolean {
    if (!plate) return false;
    if (fadeMs <= 0) {
      plate.remove();
      return true;
    }
    plate.style.transitionDuration = `${fadeMs}ms`;
    plate.setAttribute("data-leaving", "");
    schedule(() => plate.remove(), fadeMs + 60);
    return true;
  }
</script>

<script lang="ts">
  import { onMount } from "svelte";
  import { motionMs } from "../motion/tokens";

  /*
   * Renders nothing: the plate lives in index.html so it can paint before this
   * component exists. Once the shell has mounted and painted a frame, it fades
   * out on the `emphasis` token (480 ms), which is 0 under reduced motion. It
   * waits on the next frames, never on data, so a slow repository or a failed
   * load cannot hold the plate up.
   */
  onMount(() => {
    let outer = 0;
    let inner = 0;
    outer = requestAnimationFrame(() => {
      inner = requestAnimationFrame(() => {
        handOffSplash(document.getElementById(SPLASH_ID), motionMs("emphasis"));
      });
    });
    return () => {
      cancelAnimationFrame(outer);
      cancelAnimationFrame(inner);
    };
  });
</script>
