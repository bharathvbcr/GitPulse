/**
 * Conservative markdown → HTML. Escape first, then apply a small set of
 * block/inline patterns. Not a CommonMark parser; the preview must never
 * inject raw HTML from the file.
 */

export function escapeHtml(text: string): string {
  return text
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

export function renderMarkdown(text: string): string {
  if (!text) return "";
  const escaped = escapeHtml(text);
  const withBlocks = escaped
    .replace(/^### (.*)$/gim, '<h3 class="text-base font-bold text-textPrimary mt-4 mb-2">$1</h3>')
    .replace(
      /^## (.*)$/gim,
      '<h2 class="text-lg font-bold text-textPrimary mt-5 mb-2 pb-1 border-b border-border/60">$1</h2>',
    )
    .replace(
      /^# (.*)$/gim,
      '<h1 class="text-xl font-bold text-textPrimary mt-6 mb-3 pb-1 border-b border-border/80">$1</h1>',
    )
    .replace(/```[\w-]*\n([\s\S]*?)```/gim, (_full, code: string) => {
      return `<pre class="bg-surface border border-border/70 rounded-xl p-3 my-3 font-mono text-xs overflow-x-auto text-emerald-400"><code>${code}</code></pre>`;
    })
    .replace(
      /`([^`]+)`/gim,
      '<code class="bg-surface px-1.5 py-0.5 rounded text-amber-300 font-mono text-[11px] border border-border/60">$1</code>',
    )
    .replace(
      /^&gt; (.*)$/gim,
      '<blockquote class="border-l-2 border-accent/60 pl-3 py-1 my-2 text-textMuted italic bg-accent/5 rounded-r-lg">$1</blockquote>',
    )
    .replace(/\*\*([^*]+)\*\*/gim, '<strong class="font-bold text-textPrimary">$1</strong>')
    .replace(/\*([^*]+)\*/gim, '<em class="italic text-textPrimary/90">$1</em>')
    .replace(
      /^- \[x\] (.*)$/gim,
      '<div class="flex items-center gap-2 my-1 text-textPrimary"><span class="text-accent font-bold">☑</span> <span>$1</span></div>',
    )
    .replace(
      /^- \[ \] (.*)$/gim,
      '<div class="flex items-center gap-2 my-1 text-textMuted"><span class="font-bold">☐</span> <span>$1</span></div>',
    )
    .replace(/^- (.*)$/gim, '<li class="ml-4 list-disc text-textPrimary/90 my-0.5">$1</li>')
    .replace(/^---$/gim, '<hr class="border-border/60 my-4" />')
    .replace(/\n\n/gim, '</p><p class="my-2 leading-relaxed text-textPrimary/90">');

  return `<div class="prose max-w-none text-xs text-textPrimary leading-relaxed">${withBlocks}</div>`;
}
