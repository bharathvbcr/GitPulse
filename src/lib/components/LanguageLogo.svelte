<script lang="ts">
  import {
    resolveLanguageIconKey,
    getLanguageBrandColor,
    getLanguageDisplayName,
    type LanguageIconKey,
  } from "../language/languageLogos";

  let {
    language = "",
    filePath = "",
    iconKey,
    size = 14,
    class: className = "",
    title,
  }: {
    language?: string;
    filePath?: string;
    iconKey?: LanguageIconKey;
    size?: number;
    class?: string;
    title?: string;
  } = $props();

  let resolvedKey = $derived(iconKey ?? resolveLanguageIconKey(language || filePath || ""));
  let brandColor = $derived(getLanguageBrandColor(resolvedKey));
  let tip = $derived(title ?? getLanguageDisplayName(resolvedKey));
</script>

<span
  class="inline-flex items-center justify-center shrink-0 select-none {className}"
  title={tip}
  aria-label={tip}
  role="img"
>
  <svg
    xmlns="http://www.w3.org/2000/svg"
    viewBox="0 0 24 24"
    width={size}
    height={size}
    class="block"
  >
    <title>{tip}</title>
    {#if resolvedKey === "rust"}
      <!-- Rust gear logo -->
      <g fill={brandColor}>
        <path
          d="M12 2a10 10 0 0 0-3.16.52l.6 1.7A8.2 8.2 0 0 1 12 3.8a8.2 8.2 0 0 1 8.2 8.2 8.2 8.2 0 0 1-8.2 8.2 8.2 8.2 0 0 1-8.2-8.2c0-.9.15-1.78.43-2.6l-1.7-.6A10 10 0 0 0 2 12a10 10 0 0 0 10 10 10 10 0 0 0 10-10A10 10 0 0 0 12 2z"
        />
        <path
          d="M9 7.5h3.5a3 3 0 0 1 2.8 1.9 3 3 0 0 1-.8 3.2L16 16.5h-2.2l-1.3-3.5H10.8v3.5H9V7.5zm1.8 3.8h1.7a1.3 1.3 0 0 0 0-2.6h-1.7v2.6z"
        />
      </g>

    {:else if resolvedKey === "typescript"}
      <!-- TypeScript TS badge -->
      <rect width="22" height="22" x="1" y="1" rx="4" fill="#3178c6" />
      <path
        fill="#ffffff"
        d="M11.5 8.5H5.8v2.2h1.7V17h2.3v-6.3h1.7V8.5zm4.8 3.2c-1.3-.4-2-.8-2-1.5 0-.6.5-1.1 1.5-1.1 1 0 1.7.4 2 1.1l1.8-1.1c-.7-1.3-2-1.9-3.8-1.9-2.3 0-3.8 1.3-3.8 3.1 0 1.8 1.2 2.6 3.1 3.2 1.4.4 2 .9 2 1.6 0 .8-.7 1.3-1.8 1.3-1.3 0-2.1-.6-2.5-1.5L9 17.3c.7 1.5 2.1 2.2 4.3 2.2 2.6 0 4.2-1.4 4.2-3.3 0-1.9-1.2-2.7-3.2-3.3z"
      />

    {:else if resolvedKey === "javascript"}
      <!-- JavaScript JS badge -->
      <rect width="22" height="22" x="1" y="1" rx="4" fill="#f7df1e" />
      <path
        fill="#1e1e1e"
        d="M6.5 14.5c.5.8 1.1 1.3 2.1 1.3 1.1 0 1.8-.6 1.8-1.9v-5.4h2.2v5.4c0 2.4-1.4 3.7-3.9 3.7-2.1 0-3.4-1.1-4-2.5l1.8-.6zm9.8-2.8c-1.3-.4-2-.8-2-1.5 0-.6.5-1.1 1.5-1.1 1 0 1.7.4 2 1.1l1.8-1.1c-.7-1.3-2-1.9-3.8-1.9-2.3 0-3.8 1.3-3.8 3.1 0 1.8 1.2 2.6 3.1 3.2 1.4.4 2 .9 2 1.6 0 .8-.7 1.3-1.8 1.3-1.3 0-2.1-.6-2.5-1.5L9 17.3c.7 1.5 2.1 2.2 4.3 2.2 2.6 0 4.2-1.4 4.2-3.3 0-1.9-1.2-2.7-3.2-3.3z"
      />

    {:else if resolvedKey === "python"}
      <!-- Python snakes -->
      <g>
        <path
          fill="#3776ab"
          d="M11.9 2c-3.1 0-4.9 1.4-4.9 3.8v2.1h5v.7H5.2c-2.3 0-4.2 1.4-4.2 4.1 0 2.8 1.8 4.2 4.2 4.2h1.6v-2.3c0-2.3 2-4.2 4.3-4.2h4.9v-.7c0-2.5-2.1-3.7-4.1-3.7H11.9zm-1.8 1.8a.9.9 0 1 1 0 1.8.9.9 0 0 1 0-1.8z"
        />
        <path
          fill="#ffd438"
          d="M12.1 22c3.1 0 4.9-1.4 4.9-3.8v-2.1h-5v-.7h6.8c2.3 0 4.2-1.4 4.2-4.1 0-2.8-1.8-4.2-4.2-4.2h-1.6v2.3c0 2.3-2 4.2-4.3 4.2H8v.7c0 2.5 2.1 3.7 4.1 3.7h-.0zM13.9 20.2a.9.9 0 1 1 0-1.8.9.9 0 0 1 0 1.8z"
        />
      </g>

    {:else if resolvedKey === "go"}
      <!-- Go mark -->
      <g fill="#00add8">
        <ellipse cx="6" cy="12" rx="4.5" ry="6" fill="#00add8" />
        <ellipse cx="6" cy="12" rx="2.5" ry="4" fill="#ffffff" />
        <path d="M14 6h7v2.5h-7zm0 4.5h8v2.5h-8zm0 4.5h5.5v2.5H14z" />
      </g>

    {:else if resolvedKey === "svelte"}
      <!-- Svelte flame S -->
      <path
        fill="#ff3e00"
        d="M18.8 5.7c-1.5-2-4.2-3-6.9-2.5-3.5.6-6.1 3.7-5.9 7.2.1 1.2.6 2.3 1.4 3.1l-1.1 2c-.9-1-1.5-2.3-1.6-3.8-.3-4.5 3-8.4 7.5-9.1 3.5-.6 7 .8 8.9 3.4l-2.3 1.7zm-2 5.5c.1.4.1.8.1 1.2 0 3.3-2.6 6-5.9 6-1.5 0-3-.6-4-1.6l-1.6 1.7c1.4 1.4 3.5 2.3 5.6 2.3 4.7 0 8.4-3.8 8.4-8.4 0-.8-.1-1.6-.3-2.3l-2.3 1.1z"
      />

    {:else if resolvedKey === "html"}
      <!-- HTML5 shield -->
      <path fill="#e34f26" d="M3 2l1.6 18.2L12 23l7.4-2.8L21 2H3z" />
      <path fill="#ef652a" d="M12 3.8v17.4l5.9-2.2L19.3 3.8H12z" />
      <path
        fill="#ffffff"
        d="M12 7.7H7.7l.3 3.2h4V7.7zm0 5.4H9.6l.2 2.2 2.2.6v-2.8zm0-7.6h7.3l-.7 8.1-4.6 1.3v-2.3l2.5-.7.3-3.2H12V5.5z"
      />

    {:else if resolvedKey === "css"}
      <!-- CSS3 shield -->
      <path fill="#1572b6" d="M3 2l1.6 18.2L12 23l7.4-2.8L21 2H3z" />
      <path fill="#33a9dc" d="M12 3.8v17.4l5.9-2.2L19.3 3.8H12z" />
      <path
        fill="#ffffff"
        d="M12 7.7H7.7l.3 3.2h4V7.7zm0 5.4H9.6l.2 2.2 2.2.6v-2.8zm4.4-5.4H12v2.2h2.2l-.3 3.2H12v2.2l3.8-1 .4-4.4.2-2.2z"
      />

    {:else if resolvedKey === "c" || resolvedKey === "cpp" || resolvedKey === "csharp"}
      <!-- C/C++/C# shield/circle with mark -->
      <circle cx="12" cy="12" r="10" fill={brandColor} />
      <path
        fill="#ffffff"
        d="M14.5 8.5c-1-.7-2-.9-3.2-.9-2.7 0-4.6 1.8-4.6 4.4 0 2.5 1.9 4.4 4.6 4.4 1.3 0 2.3-.3 3.2-.9l.7 1.8c-1.1.7-2.4 1-4 1-3.9 0-6.8-2.6-6.8-6.3 0-3.8 2.9-6.3 6.8-6.3 1.6 0 3 .3 4 1l-.7 1.8z"
      />
      {#if resolvedKey === "cpp"}
        <path fill="#ffffff" d="M16 11h1v-1h1v1h1v1h-1v1h-1v-1h-1v-1zm4 0h1v-1h1v1h1v1h-1v1h-1v-1h-1v-1z" />
      {:else if resolvedKey === "csharp"}
        <path fill="#ffffff" d="M17 10h1.5v1H17v2h1.5v1H17v1.5h-1V14h-2v1.5h-1V14h-1v-1h1v-2h-1v-1h1V8.5h1V10h2V8.5h1V10zm-3 1v2h2v-2h-2z" />
      {/if}

    {:else if resolvedKey === "java"}
      <!-- Java cup -->
      <path
        fill="#b07219"
        d="M4 10h12v4.5c0 2.5-2 4.5-4.5 4.5h-3C6 19 4 17 4 14.5V10zm13 1h1.5a2 2 0 0 1 0 4H17v-4zM3 21h15v1.5H3V21zM10 3c-1.5 2 0 3.5-1 5H8c0-2-1.5-3.5 0-5h2zm4 0c-1.5 2 0 3.5-1 5h-1c0-2-1.5-3.5 0-5h2z"
      />

    {:else if resolvedKey === "ruby"}
      <!-- Ruby gem -->
      <path
        fill="#e0115f"
        d="M3 9l4.5-6h9L21 9l-9 13L3 9zm3.5-4.5L4.8 8h4.4L7.8 4.5H6.5zm3.5 0L11.5 8h-3l1.5-3.5zm4 0L12.5 8h3l-1.5-3.5zm2 0L14.8 8h4.4l-1.7-3.5h-1.5zM4.3 9.5l7 10.1V9.5H4.3zm8.4 0v10.1l7-10.1h-7z"
      />

    {:else if resolvedKey === "php"}
      <!-- PHP oval -->
      <ellipse cx="12" cy="12" rx="10.5" ry="6.5" fill="#4f5d95" />
      <path
        fill="#ffffff"
        d="M6 10h2.5c1.2 0 1.8.6 1.8 1.5 0 1-.6 1.5-1.8 1.5H7.2V15H6V10zm1.2 2.1h1.2c.5 0 .8-.2.8-.6 0-.4-.3-.6-.8-.6H7.2v1.2zm4.8-2.1h1.2v2h2v-2h1.2V15h-1.2v-2h-2v2H12V10zm6 0h2.5c1.2 0 1.8.6 1.8 1.5 0 1-.6 1.5-1.8 1.5h-1.3V15H18V10zm1.2 2.1h1.2c.5 0 .8-.2.8-.6 0-.4-.3-.6-.8-.6h-1.2v1.2z"
      />

    {:else if resolvedKey === "swift"}
      <!-- Swift bird -->
      <path
        fill="#f05138"
        d="M21.5 13.8c-.8.8-2.5 1.5-4 1.7 1.8-.8 3.3-2.3 3.8-3.7-2.3 1.8-5.3 2.5-8.2 2 3-1.8 4.7-4.8 4.5-7.8-2.3 2.5-5.6 4.3-8.8 4.7 1.7-1.8 2.8-4.2 2.5-6.7-3.2 3.3-7.5 7.7-8.8 13.5 3-2.3 6.8-3.5 10.7-3.2-3.8 2.2-6.5 5.8-7.2 10.2 4.5-3.5 10.2-4.5 15.5-2.2 0-.2-.1-.4-.2-.5 1.7-.5 3.3-1.5 4.5-2.8l-4.3-5.2z"
      />

    {:else if resolvedKey === "kotlin"}
      <!-- Kotlin flag/shapes -->
      <path fill="#7f52ff" d="M22 2H2v20h20L12 12l10-10z" />
      <path fill="#c711e1" d="M2 22l10-10L2 2v20z" />
      <path fill="#e4485d" d="M12 12L2 22h20L12 12z" />

    {:else if resolvedKey === "dart"}
      <!-- Dart chevron -->
      <path fill="#00b4ab" d="M4 14l6 6 10-10L14 4H4v10z" />
      <path fill="#0175c2" d="M14 4l-4 4 6 6 4-4-6-6z" />
      <path fill="#02569b" d="M4 14l6 6h4l-6-6H4z" />

    {:else if resolvedKey === "shell"}
      <!-- Terminal prompt >_ -->
      <rect width="20" height="16" x="2" y="4" rx="3" fill="#1e1e1e" stroke={brandColor} stroke-width="1.5" />
      <path fill="none" stroke={brandColor} stroke-width="2" stroke-linecap="round" stroke-linejoin="round" d="M6 9l3 3-3 3M12 15h5" />

    {:else if resolvedKey === "sql"}
      <!-- Database cylinder -->
      <ellipse cx="12" cy="6" rx="8" ry="3" fill={brandColor} />
      <path
        fill={brandColor}
        d="M4 6v5c0 1.66 3.58 3 8 3s8-1.34 8-3V6c0 1.66-3.58 3-8 3S4 7.66 4 6z"
      />
      <path
        fill={brandColor}
        d="M4 13v5c0 1.66 3.58 3 8 3s8-1.34 8-3v-5c0 1.66-3.58 3-8 3s-8-1.34-8-3z"
      />

    {:else if resolvedKey === "json"}
      <!-- Curly braces {} -->
      <rect width="20" height="18" x="2" y="3" rx="3" fill="#2d3748" />
      <text x="12" y="16" font-size="13" font-weight="bold" fill="#f6ad55" text-anchor="middle" font-family="monospace">&#123;&#125;</text>

    {:else if resolvedKey === "yaml" || resolvedKey === "toml" || resolvedKey === "config"}
      <!-- Config / settings sliders -->
      <rect width="20" height="18" x="2" y="3" rx="3" fill="#374151" />
      <path
        fill="none"
        stroke={brandColor}
        stroke-width="1.8"
        stroke-linecap="round"
        d="M6 8h12M6 12h12M6 16h12M9 6v4M15 10v4M11 14v4"
      />

    {:else if resolvedKey === "markdown"}
      <!-- Markdown M logo -->
      <rect width="22" height="16" x="1" y="4" rx="2.5" fill="#083fa1" />
      <path
        fill="#ffffff"
        d="M4 8v8h2.2v-4.5L8 14l1.8-2.5V16H12V8H9.8L8 10.5 6.2 8H4zm12.5 0l-3 4h2v4h2v-4h2l-3-4z"
      />

    {:else if resolvedKey === "docker"}
      <!-- Docker whale -->
      <g fill="#2496ed">
        <rect x="5" y="8" width="2" height="2" rx=".3" />
        <rect x="8" y="8" width="2" height="2" rx=".3" />
        <rect x="11" y="8" width="2" height="2" rx=".3" />
        <rect x="8" y="5.5" width="2" height="2" rx=".3" />
        <rect x="11" y="5.5" width="2" height="2" rx=".3" />
        <rect x="14" y="8" width="2" height="2" rx=".3" />
        <path d="M22 11.5c-.7 0-1.5.3-2.1.8-.7-.2-1.8-.1-2.9.5-1.5-.2-4.5 0-6 2.2H3c0 4 3 6.5 7.5 6.5 6 0 10.5-3.5 11.5-8.5.5-.3 1-.7 1.5-1.5h-1.5z" />
      </g>

    {:else if resolvedKey === "git"}
      <!-- Git branch -->
      <path
        fill="#f05032"
        d="M21.7 10.6l-8.3-8.3a2.5 2.5 0 0 0-3.5 0L7.4 4.8l2.9 2.9a2.2 2.2 0 0 1 2.8.5 2.2 2.2 0 0 1 .5 2.8l2.8 2.8a2.2 2.2 0 0 1 2.8.5 2.2 2.2 0 0 1 .5 2.8l3.1 3.1a2.5 2.5 0 0 0 3.5 0l4.4-4.4a2.5 2.5 0 0 0 0-3.5l-8.3-8.3zm-7.6 8.5a1.2 1.2 0 1 1 0-2.4 1.2 1.2 0 0 1 0 2.4zm-4.7-9.5a1.2 1.2 0 1 1 0-2.4 1.2 1.2 0 0 1 0 2.4z"
      />

    {:else if resolvedKey === "lock"}
      <!-- Padlock -->
      <rect width="14" height="11" x="5" y="10" rx="2" fill="#6b7280" />
      <path
        fill="none"
        stroke="#6b7280"
        stroke-width="2"
        d="M8 10V6.5a4 4 0 0 1 8 0V10"
      />
      <circle cx="12" cy="15" r="1.2" fill="#ffffff" />

    {:else if resolvedKey === "image" || resolvedKey === "svg"}
      <!-- Image/picture icon -->
      <rect width="18" height="15" x="3" y="4.5" rx="2.5" fill="none" stroke={brandColor} stroke-width="1.8" />
      <circle cx="8" cy="9" r="1.5" fill={brandColor} />
      <path fill={brandColor} d="M4.5 17l4.5-5 3.5 3.5 3-3.5 4 5H4.5z" />

    {:else if resolvedKey === "archive"}
      <!-- Archive package box -->
      <rect width="18" height="15" x="3" y="5" rx="2" fill="#d48434" />
      <path fill="#ffffff" d="M11 5h2v7h-2zM9 13h6v2H9z" />

    {:else}
      <!-- Clean document fallback -->
      <path
        fill="#4b5563"
        d="M6 3a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8.5L14.5 3H6zm8 1.5L18.5 9H14V4.5zM7 12h10v1.5H7V12zm0 3h7v1.5H7V15z"
      />
    {/if}
  </svg>
</span>
