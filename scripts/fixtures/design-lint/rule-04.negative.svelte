<script lang="ts">
  // #fff in a comment
  const canvasInk = "#8b949e"; // adapters convert tokens at their boundary
</script>

<!-- color: #ff0000 in an HTML comment -->
<a href="#abc">jump</a>
<svg><use href="#def" /><rect fill="url(#bad)" /></svg>
<p>Fixed in PR #123 and &#39;quoted&#39;</p>
<button onclick={() => document.querySelector("#add")?.focus()}>focus</button>

<style>
  /* color: #fff in a CSS comment */
  #add {
    color: red;
  }
  .a {
    color: rgb(var(--c-text) / 0.5);
    background: rgb(var(--c-surface));
  }
</style>
