/**
 * SVG generator for the exportable repository Pulse summary card.
 *
 * The card leaves the app. It is pasted into READMEs, wikis and issues where
 * nobody can ask what a number meant, so three rules hold here that do not
 * hold for an in-app tile:
 *
 *  1. Every tile states its own definition. A label alone ("Bus Factor") is
 *     not a definition.
 *  2. Every caveat sits on the tile it applies to, not in a global footnote.
 *  3. A metric whose scan did not run renders as an em dash and says so. It is
 *     never rendered as 0, which reads exactly like a measurement that
 *     succeeded and found nothing.
 *
 * The output is pure ASCII (non-ASCII copy is emitted as numeric character
 * references) so the file renders identically regardless of how the consuming
 * renderer guesses the encoding, and it avoids the CSS features that browsers
 * support but standalone SVG rasterisers do not (`text-transform`, geometry
 * properties such as `rx`).
 */

/**
 * A metric that may not have been measured.
 *
 * `null` means "the scan behind this number did not produce an answer" and is
 * rendered as such. It is deliberately not collapsed to 0 anywhere below.
 */
export type CardValue = number | null;

export interface ExportCardOptions {
  readonly repoName: string;
  /** Commits in the summarised window, after any author scope is applied. */
  readonly totalCommits: number;
  /** Distinct local calendar days carrying at least one of those commits. */
  readonly activeDays: CardValue;
  /** Earliest commit day in the window, `YYYY-MM-DD`. */
  readonly windowStart?: string | null;
  /** Latest commit day in the window, `YYYY-MM-DD`. */
  readonly windowEnd?: string | null;
  /** Author the window was narrowed to. Omitted or null means everyone. */
  readonly authorScope?: string | null;
  /** True when the commit scan stopped before the repository's first commit. */
  readonly truncated?: boolean;
  /** Code lines in the working tree, or null when the language scan failed. */
  readonly totalLoc: CardValue;
  /** True when the language scan was itself capped, making LOC a floor. */
  readonly locPartial?: boolean;
  /** Contributors owning half the surviving lines, or null if blame did not run. */
  readonly busFactor: CardValue;
  /** Days within which half the surviving lines were last touched. */
  readonly halfLifeDays: CardValue;
  /** True when the blame scan covered only part of the candidate files. */
  readonly blamePartial?: boolean;
  /** Share of commit subjects parsing as Conventional Commits. */
  readonly conventionalPct: CardValue;
  /** Share of commits git reports as carrying a good signature. */
  readonly signedPct: CardValue;
  /** Stamp shown in the header, `YYYY-MM-DD`. Defaults to today. */
  readonly generatedDate?: string;
}

const PAD = 32;
const TILE_W = 240;
const TILE_GAP = 18;
const TILE_H = 132;
const TILE_PAD = 16;
const CARD_WIDTH = PAD * 2 + TILE_W * 3 + TILE_GAP * 2;

const HEADER_RULE_Y = 110;
const ROW_A_LABEL_Y = 132;
const ROW_A_Y = 142;
const ROW_B_LABEL_Y = ROW_A_Y + TILE_H + 28;
const ROW_B_Y = ROW_B_LABEL_Y + 10;
const FOOTER_RULE_Y = ROW_B_Y + TILE_H + 22;
const FOOTER_TEXT_Y = FOOTER_RULE_Y + 20;
const CARD_HEIGHT = FOOTER_TEXT_Y + 18;

/** Characters that fit one meaning line at 11.5px in the card's font stack. */
const MEANING_CHARS = 34;
const MEANING_MAX_LINES = 3;
/** Characters that fit the header before the right-hand brand block. */
const NAME_CHARS = 44;
const SCOPE_CHARS = 118;

const COLOR = {
  bgFrom: "#0b1220",
  bgTo: "#131d31",
  border: "#27324a",
  tile: "#151f33",
  tileBorder: "#26314a",
  heading: "#f8fafc",
  value: "#f1f5f9",
  unmeasured: "#64748b",
  label: "#8ba0bd",
  body: "#93a4bd",
  faint: "#6b7c96",
  accent: "#38bdf8",
  good: "#34d399",
  warn: "#fbbf24",
  bad: "#f87171",
} as const;

const EM_DASH = "—";
const EN_DASH = "–";
const MONTHS = [
  "Jan", "Feb", "Mar", "Apr", "May", "Jun",
  "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
] as const;

interface Chip {
  readonly text: string;
  readonly color: string;
}

interface Tile {
  readonly label: string;
  readonly value: string;
  readonly unit?: string;
  readonly measured: boolean;
  readonly chip?: Chip | null;
  /** Plain-English definition of the number, wrapped into the tile. */
  readonly meaning: string;
}

/**
 * Escape for XML text content and fold every non-ASCII code point to a numeric
 * character reference, so the document carries no encoding assumptions.
 */
function xmlText(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&apos;")
    .replace(/[^\x20-\x7E]/gu, (ch) => `&#${ch.codePointAt(0)};`);
}

/** Fixed en-US grouping: the card is a file, not a locale-sensitive view. */
function formatCount(value: number): string {
  return Number.isFinite(value) ? Math.round(value).toLocaleString("en-US") : EM_DASH;
}

function plural(count: number, singular: string, suffix = "s"): string {
  return count === 1 ? singular : `${singular}${suffix}`;
}

function truncateText(value: string, maxChars: number): string {
  return value.length <= maxChars ? value : `${value.slice(0, Math.max(1, maxChars - 1))}…`;
}

/**
 * Greedy word wrap into at most `maxLines` lines of `maxChars`, ellipsising
 * the final line when copy overflows. SVG has no reflow, so this is the only
 * thing standing between a long sentence and a line that runs off the tile.
 */
function fitLines(text: string, maxChars = MEANING_CHARS, maxLines = MEANING_MAX_LINES): string[] {
  const words = text.split(/\s+/).filter(Boolean);
  const lines: string[] = [];
  let current = "";
  let overflow = false;

  for (const word of words) {
    const chunk = truncateText(word, maxChars);
    const candidate = current ? `${current} ${chunk}` : chunk;
    if (candidate.length <= maxChars) {
      current = candidate;
      continue;
    }
    if (lines.length === maxLines - 1) {
      overflow = true;
      break;
    }
    lines.push(current);
    current = chunk;
  }
  if (current) lines.push(current);

  if (overflow && lines.length > 0) {
    const last = lines[lines.length - 1];
    lines[lines.length - 1] =
      last.length >= maxChars ? `${last.slice(0, maxChars - 1)}…` : `${last}…`;
  }
  return lines;
}

/** `2026-09-03` to `3 Sep 2026`, without going through Date's UTC parsing. */
function formatDay(iso: string | null | undefined): string | null {
  if (!iso) return null;
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(iso.trim());
  if (!match) return null;
  const month = Number(match[2]);
  const day = Number(match[3]);
  if (month < 1 || month > 12 || day < 1 || day > 31) return null;
  return `${day} ${MONTHS[month - 1]} ${match[1]}`;
}

function formatRange(start: string | null | undefined, end: string | null | undefined): string | null {
  const from = formatDay(start);
  const to = formatDay(end);
  if (from && to) return from === to ? from : `${from} ${EN_DASH} ${to}`;
  return from ?? to ?? null;
}

/** Mirrors the in-app bus factor rating so the card and the view agree. */
function busFactorChip(busFactor: number): Chip {
  if (busFactor <= 1) return { text: "CRITICAL", color: COLOR.bad };
  if (busFactor === 2) return { text: "MODERATE", color: COLOR.warn };
  return { text: "HEALTHY", color: COLOR.good };
}

function unmeasured(label: string, reason: string, chip: Chip | null = null): Tile {
  return { label, value: EM_DASH, measured: false, chip, meaning: reason };
}

/**
 * Chip vocabulary is kept short on purpose: a chip is right-aligned inside the
 * tile and a long one collides with the longest label ("CONVENTIONAL COMMITS").
 */
const NOT_MEASURED: Chip = { text: "UNSCANNED", color: COLOR.faint };
const PARTIAL: Chip = { text: "PARTIAL", color: COLOR.warn };
const CAPPED: Chip = { text: "CAPPED", color: COLOR.warn };

function commitsTile(options: ExportCardOptions): Tile {
  const { totalCommits, activeDays, truncated } = options;
  const spread =
    activeDays === null || activeDays === undefined
      ? ""
      : ` Spread over ${formatCount(activeDays)} active ${plural(activeDays, "day")}.`;
  const cap = truncated ? " Older history was not scanned." : "";
  return {
    label: "COMMITS",
    value: formatCount(totalCommits),
    measured: true,
    chip: truncated ? CAPPED : null,
    meaning: `Commits in scope, merges included.${spread}${cap}`,
  };
}

function locTile(options: ExportCardOptions): Tile {
  if (options.totalLoc === null || options.totalLoc === undefined) {
    return unmeasured(
      "LINES OF CODE",
      "The language scan did not complete. This is not a count of zero.",
      NOT_MEASURED,
    );
  }
  return {
    label: "LINES OF CODE",
    value: formatCount(options.totalLoc),
    measured: true,
    chip: options.locPartial ? PARTIAL : null,
    meaning: options.locPartial
      ? "Code lines in the working tree. The scan was capped, so this is a floor."
      : "Code lines in the working tree today. Blank lines and comments excluded.",
  };
}

function halfLifeTile(options: ExportCardOptions): Tile {
  if (options.halfLifeDays === null || options.halfLifeDays === undefined) {
    return unmeasured(
      "CODE HALF-LIFE",
      "The blame scan has not completed. This is not an age of zero.",
      NOT_MEASURED,
    );
  }
  const days = Math.max(0, Math.round(options.halfLifeDays));
  return {
    label: "CODE HALF-LIFE",
    value: formatCount(days),
    unit: plural(days, "day"),
    measured: true,
    chip: options.blamePartial ? PARTIAL : null,
    meaning: `Half the live code was last touched within the past ${formatCount(days)} ${plural(days, "day")}.`,
  };
}

function busFactorTile(options: ExportCardOptions): Tile {
  if (options.busFactor === null || options.busFactor === undefined) {
    return unmeasured(
      "BUS FACTOR",
      "The blame scan has not completed. This is not a bus factor of zero.",
      NOT_MEASURED,
    );
  }
  const busFactor = Math.max(0, Math.round(options.busFactor));
  return {
    label: "BUS FACTOR",
    value: formatCount(busFactor),
    unit: plural(busFactor, "contributor"),
    measured: true,
    chip: options.blamePartial ? PARTIAL : busFactorChip(busFactor),
    meaning:
      busFactor === 1
        ? "One person alone owns half the surviving lines. Higher is safer."
        : `${formatCount(busFactor)} contributors together own half the surviving lines. Higher is safer.`,
  };
}

/**
 * Percentage tiles carry no chip: their labels are the widest on the card, and
 * the em dash plus the reason already say everything a chip would.
 */
function percentTile(label: string, value: CardValue, meaning: string): Tile {
  if (value === null || value === undefined) {
    return unmeasured(label, "No commits in scope, so there was nothing to measure.");
  }
  return {
    label,
    value: formatCount(Math.max(0, Math.min(100, value))),
    unit: "%",
    measured: true,
    meaning,
  };
}

function buildScopeLine(options: ExportCardOptions): string {
  const commits = `${formatCount(options.totalCommits)} ${plural(options.totalCommits, "commit")}`;
  const parts = [options.truncated ? `Most recent ${commits}` : commits];
  const range = formatRange(options.windowStart, options.windowEnd);
  if (range) parts.push(range);
  parts.push(options.authorScope ? `Author: ${options.authorScope}` : "All contributors");
  return truncateText(parts.join("  ·  "), SCOPE_CHARS);
}

function renderChip(chip: Chip): string {
  const width = Math.round(chip.text.length * 6.3) + 16;
  const x = TILE_W - 14 - width;
  return `
    <rect x="${x}" y="12" width="${width}" height="19" rx="9" fill="${chip.color}" fill-opacity="0.14" stroke="${chip.color}" stroke-opacity="0.4" stroke-width="1" />
    <text x="${x + width / 2}" y="25.5" class="gp-chip" text-anchor="middle" fill="${chip.color}">${xmlText(chip.text)}</text>`;
}

/** Shrink the headline so long counts stay inside the tile. */
function valueFontSize(value: string): number {
  if (value.length >= 10) return 21;
  if (value.length >= 8) return 24;
  return 28;
}

function renderTile(tile: Tile, x: number, y: number): string {
  const valueFill = tile.measured ? COLOR.value : COLOR.unmeasured;
  // A percent sign belongs against its number; a word unit needs breathing room.
  const unit = tile.unit
    ? `<tspan class="gp-unit" dx="${tile.unit === "%" ? 2 : 6}" fill="${COLOR.body}">${xmlText(tile.unit)}</tspan>`
    : "";
  const meaning = fitLines(tile.meaning)
    .map(
      (line, index) =>
        `    <text x="${TILE_PAD}" y="${92 + index * 16}" class="gp-mean">${xmlText(line)}</text>`,
    )
    .join("\n");

  return `  <g transform="translate(${x}, ${y})">
    <rect width="${TILE_W}" height="${TILE_H}" rx="10" fill="${COLOR.tile}" stroke="${COLOR.tileBorder}" stroke-width="1" />
    <text x="${TILE_PAD}" y="26" class="gp-label">${xmlText(tile.label)}</text>${tile.chip ? renderChip(tile.chip) : ""}
    <text x="${TILE_PAD}" y="66" class="gp-value" font-size="${valueFontSize(tile.value)}" fill="${valueFill}">${xmlText(tile.value)}${unit}</text>
${meaning}
  </g>`;
}

function renderRow(tiles: readonly Tile[], y: number): string {
  return tiles
    .map((tile, index) => renderTile(tile, PAD + index * (TILE_W + TILE_GAP), y))
    .join("\n");
}

function renderGroupLabel(text: string, y: number): string {
  return `  <text x="${PAD}" y="${y}" class="gp-group">${xmlText(text)}</text>`;
}

export function generatePulseSvgCard(options: ExportCardOptions): string {
  const generatedDate = options.generatedDate ?? new Date().toISOString().slice(0, 10);
  const stamp = formatDay(generatedDate) ?? generatedDate;
  const repoName = truncateText(options.repoName?.trim() || "repository", NAME_CHARS);

  const logRow: Tile[] = [
    commitsTile(options),
    percentTile(
      "CONVENTIONAL COMMITS",
      options.conventionalPct,
      "Share of subjects in Conventional Commits form: feat:, fix:, docs:.",
    ),
    percentTile(
      "SIGNED COMMITS",
      options.signedPct,
      "Share of commits git reports as carrying a good signature.",
    ),
  ];
  const codeRow: Tile[] = [locTile(options), halfLifeTile(options), busFactorTile(options)];

  const title = `${repoName} repository pulse`;
  const description =
    `Pulse summary card for ${repoName}. ${buildScopeLine(options)}. ` +
    [...logRow, ...codeRow]
      .map((tile) => `${tile.label}: ${tile.value}${tile.unit ? ` ${tile.unit}` : ""}. ${tile.meaning}`)
      .join(" ");

  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${CARD_WIDTH} ${CARD_HEIGHT}" width="${CARD_WIDTH}" height="${CARD_HEIGHT}" class="gp-pulse-card" role="img" aria-label="${xmlText(title)}">
  <title>${xmlText(title)}</title>
  <desc>${xmlText(description)}</desc>
  <defs>
    <linearGradient id="gp-pulse-bg" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="${COLOR.bgFrom}" />
      <stop offset="100%" stop-color="${COLOR.bgTo}" />
    </linearGradient>
    <style>
      svg.gp-pulse-card text { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif; }
      svg.gp-pulse-card .gp-name { font-size: 22px; font-weight: 700; fill: ${COLOR.heading}; }
      svg.gp-pulse-card .gp-tagline { font-size: 12px; font-weight: 500; fill: ${COLOR.body}; }
      svg.gp-pulse-card .gp-scope { font-size: 11.5px; font-weight: 500; fill: ${COLOR.label}; }
      svg.gp-pulse-card .gp-brand { font-size: 11px; font-weight: 700; letter-spacing: 0.14em; fill: ${COLOR.accent}; }
      svg.gp-pulse-card .gp-stamp { font-size: 10.5px; font-weight: 500; fill: ${COLOR.faint}; }
      svg.gp-pulse-card .gp-group { font-size: 10.5px; font-weight: 700; letter-spacing: 0.12em; fill: ${COLOR.label}; }
      svg.gp-pulse-card .gp-label { font-size: 10.5px; font-weight: 700; letter-spacing: 0.08em; fill: ${COLOR.label}; }
      svg.gp-pulse-card .gp-value { font-weight: 800; }
      svg.gp-pulse-card .gp-unit { font-size: 12px; font-weight: 600; }
      svg.gp-pulse-card .gp-mean { font-size: 11.5px; font-weight: 400; fill: ${COLOR.body}; }
      svg.gp-pulse-card .gp-chip { font-size: 9px; font-weight: 700; letter-spacing: 0.06em; }
      svg.gp-pulse-card .gp-foot { font-size: 10.5px; font-weight: 500; fill: ${COLOR.faint}; }
    </style>
  </defs>

  <rect width="${CARD_WIDTH}" height="${CARD_HEIGHT}" rx="14" fill="url(#gp-pulse-bg)" stroke="${COLOR.border}" stroke-width="1.5" />

  <g transform="translate(${PAD}, 0)">
    <circle cx="14" cy="44" r="14" fill="${COLOR.accent}" fill-opacity="0.14" stroke="${COLOR.accent}" stroke-width="2" />
    <path d="M 5 44 L 10 44 L 13 36 L 16 52 L 19 44 L 23 44" fill="none" stroke="${COLOR.accent}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
    <text x="44" y="52" class="gp-name">${xmlText(repoName)}</text>
    <text x="44" y="74" class="gp-tagline">Repository Pulse ${xmlText(EM_DASH)} measured from this repository's git history and working tree.</text>
    <text x="44" y="93" class="gp-scope">${xmlText(buildScopeLine(options))}</text>
  </g>
  <text x="${CARD_WIDTH - PAD}" y="48" class="gp-brand" text-anchor="end">GITPULSE</text>
  <text x="${CARD_WIDTH - PAD}" y="66" class="gp-stamp" text-anchor="end">Generated ${xmlText(stamp)}</text>
  <line x1="${PAD}" y1="${HEADER_RULE_Y}" x2="${CARD_WIDTH - PAD}" y2="${HEADER_RULE_Y}" stroke="${COLOR.border}" stroke-width="1" />

${renderGroupLabel("FROM THE COMMIT LOG", ROW_A_LABEL_Y)}
${renderRow(logRow, ROW_A_Y)}

${renderGroupLabel("FROM THE CODE ITSELF  ·  GIT BLAME AND WORKING TREE", ROW_B_LABEL_Y)}
${renderRow(codeRow, ROW_B_Y)}

  <line x1="${PAD}" y1="${FOOTER_RULE_Y}" x2="${CARD_WIDTH - PAD}" y2="${FOOTER_RULE_Y}" stroke="${COLOR.border}" stroke-width="1" />
  <text x="${PAD}" y="${FOOTER_TEXT_Y}" class="gp-foot">${xmlText(
    `A metric whose scan did not run shows ${EM_DASH}, never 0. Measured offline by GitPulse; no repository data leaves this machine.`,
  )}</text>
</svg>`;
}
