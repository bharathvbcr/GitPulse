/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{svelte,js,ts,jsx,tsx}"],
  darkMode: "class",
  theme: {
    extend: {
      colors: {
        // RGB-triplet channels (see src/app.css) so opacity modifiers
        // (`bg-accent/15`, `border-border/60`) compile correctly.
        background: "rgb(var(--c-bg) / <alpha-value>)",
        surface: "rgb(var(--c-surface) / <alpha-value>)",
        surfaceHover: "rgb(var(--c-surface-hover) / <alpha-value>)",
        border: "rgb(var(--c-border) / <alpha-value>)",
        accent: "rgb(var(--c-accent) / <alpha-value>)",
        textPrimary: "rgb(var(--c-text) / <alpha-value>)",
        textMuted: "rgb(var(--c-text-muted) / <alpha-value>)",
      },
      boxShadow: {
        card: "var(--shadow-card)",
        pop: "var(--shadow-pop)",
        float: "var(--shadow-float)",
        glow: "var(--shadow-glow)",
      },
    },
  },
  plugins: [],
};
