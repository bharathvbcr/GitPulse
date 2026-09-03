/**
 * SVG generator for exportable repository Pulse summary cards.
 *
 * Produces a self-contained, valid SVG image suitable for inclusion in
 * GitHub READMEs, documentation, or local saving.
 */

export interface ExportCardOptions {
  repoName: string;
  totalCommits: number;
  totalLoc: number;
  activeDays: number;
  busFactor: number;
  halfLifeDays: number;
  conventionalPct: number;
  signedPct: number;
  generatedDate?: string;
}

export function generatePulseSvgCard(options: ExportCardOptions): string {
  const {
    repoName,
    totalCommits,
    totalLoc,
    activeDays,
    busFactor,
    halfLifeDays,
    conventionalPct,
    signedPct,
    generatedDate = new Date().toISOString().slice(0, 10),
  } = options;

  const busFactorColor = busFactor === 1 ? '#f87171' : busFactor === 2 ? '#fbbf24' : '#34d399';

  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 760 380" width="760" height="380">
  <defs>
    <linearGradient id="bg" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#0f172a" />
      <stop offset="100%" stop-color="#1e293b" />
    </linearGradient>
    <style>
      .title { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; font-size: 20px; font-weight: 700; fill: #f8fafc; }
      .subtitle { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; font-size: 11px; fill: #94a3b8; font-weight: 500; }
      .badge-label { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; font-size: 11px; fill: #64748b; font-weight: 600; text-transform: uppercase; letter-spacing: 0.05em; }
      .badge-value { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; font-size: 24px; font-weight: 800; fill: #f1f5f9; }
      .subtext { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; font-size: 12px; fill: #94a3b8; }
      .card-box { fill: #1e293b; stroke: #334155; stroke-width: 1; rx: 8; }
    </style>
  </defs>

  <!-- Background -->
  <rect width="760" height="380" rx="12" fill="url(#bg)" stroke="#334155" stroke-width="1.5" />

  <!-- Header -->
  <g transform="translate(32, 40)">
    <circle cx="12" cy="12" r="10" fill="#38bdf8" fill-opacity="0.2" stroke="#38bdf8" stroke-width="2" />
    <path d="M 6 12 L 10 12 L 12 7 L 14 17 L 16 12 L 18 12" fill="none" stroke="#38bdf8" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
    <text x="32" y="17" class="title">${escapeXml(repoName)}</text>
    <text x="32" y="34" class="subtitle">GitPulse Repository Intelligence Snapshot • Generated ${escapeXml(generatedDate)}</text>
  </g>

  <!-- 6 Key Metric Tiles -->
  <!-- Row 1 -->
  <!-- Commits -->
  <g transform="translate(32, 95)">
    <rect width="216" height="105" class="card-box" />
    <text x="20" y="32" class="badge-label">Commits Scanned</text>
    <text x="20" y="68" class="badge-value">${totalCommits.toLocaleString()}</text>
    <text x="20" y="88" class="subtext">${activeDays} active days</text>
  </g>

  <!-- Total LOC -->
  <g transform="translate(272, 95)">
    <rect width="216" height="105" class="card-box" />
    <text x="20" y="32" class="badge-label">Total Code Lines</text>
    <text x="20" y="68" class="badge-value">${totalLoc.toLocaleString()}</text>
    <text x="20" y="88" class="subtext">Measured across repo</text>
  </g>

  <!-- Bus Factor -->
  <g transform="translate(512, 95)">
    <rect width="216" height="105" class="card-box" />
    <text x="20" y="32" class="badge-label">Bus Factor</text>
    <text x="20" y="68" class="badge-value" fill="${busFactorColor}">${busFactor}</text>
    <text x="20" y="88" class="subtext">${busFactor === 1 ? 'High risk (1 key author)' : 'Team distribution'}</text>
  </g>

  <!-- Row 2 -->
  <!-- Code Half-Life -->
  <g transform="translate(32, 220)">
    <rect width="216" height="105" class="card-box" />
    <text x="20" y="32" class="badge-label">Code Half-Life</text>
    <text x="20" y="68" class="badge-value">${halfLifeDays > 0 ? `${halfLifeDays}d` : 'N/A'}</text>
    <text x="20" y="88" class="subtext">Median line age</text>
  </g>

  <!-- Conventional Commits -->
  <g transform="translate(272, 220)">
    <rect width="216" height="105" class="card-box" />
    <text x="20" y="32" class="badge-label">Conventional Commits</text>
    <text x="20" y="68" class="badge-value">${conventionalPct}%</text>
    <text x="20" y="88" class="subtext">Structured commit messages</text>
  </g>

  <!-- Signed Commits -->
  <g transform="translate(512, 220)">
    <rect width="216" height="105" class="card-box" />
    <text x="20" y="32" class="badge-label">GPG Signed Commits</text>
    <text x="20" y="68" class="badge-value">${signedPct}%</text>
    <text x="20" y="88" class="subtext">Cryptographic verification</text>
  </g>

  <!-- Footer -->
  <g transform="translate(32, 352)">
    <text x="0" y="0" font-family="-apple-system, BlinkMacSystemFont, sans-serif" font-size="10" fill="#475569">
      Generated offline with GitPulse • Honesty invariant preserved
    </text>
  </g>
</svg>`;
}

function escapeXml(unsafe: string): string {
  return unsafe
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&apos;');
}
