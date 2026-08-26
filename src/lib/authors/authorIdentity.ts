/**
 * Canonical author-avatar identity.
 *
 * One owner for everything an avatar needs: deterministic hue, display
 * initials, and the identity key they were derived from. Previously every
 * surface (commit rows, tooltips) rolled its own hash — the row variant
 * hashed only the display name, so two authors sharing a name collapsed to
 * one colour while one author with a changed display name split in two.
 * Keying on email-then-name fixes both, and sharing this module keeps the
 * canvas gutters, DOM rows, and tooltips visually consistent.
 *
 * All inputs are untrusted: git allows empty names, emails made entirely of
 * punctuation, lone surrogates, RTL overrides, 4 MB strings. Everything here
 * must return a finite, bounded, allocation-light result for any input.
 */

export interface AuthorIdentity {
  /** Canonical identity source: trimmed email, else trimmed name. */
  key: string;
  /** 0–359 deterministic hue derived from the key. */
  hue: number;
  /** 1–2 grapheme clusters, uppercased where casing applies; "?" when unknown. */
  initials: string;
}

/** Avatar fill for a hue; matches the DOM avatar treatment (h/s/l fixed). */
export function authorColor(hue: number): string {
  const h = Number.isFinite(hue) ? ((Math.trunc(hue) % 360) + 360) % 360 : 0;
  return `hsl(${h}, 62%, 45%)`;
}

/**
 * FNV-1a over UTF-16 code units. Chosen over the old `(hash << 5) - hash`
 * because it has no degenerate short-string clustering (names like "a", "b",
 * "c" used to land within a few degrees of hue).
 */
function fnv1a(input: string): number {
  let hash = 0x811c9dc5;
  for (let i = 0; i < input.length; i++) {
    hash ^= input.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  // >>> keeps the result unsigned so the golden-angle spread below cannot go
  // negative for high hashes.
  return hash >>> 0;
}

/**
 * Full 32-bit hash through the golden angle: consecutive hashes land ~137.5°
 * apart, so sequentially-numbered authors (user1@, user2@, … — the norm in
 * generated/synced repos) get maximally distinct colours. The multiply stays
 * exact in float64 (2^32 × 137.508 < 2^53).
 */
function hueFromHash(hash: number): number {
  return (((hash >>> 0) * 137.508) % 360 + 360) % 360;
}

/** Grapheme splitter with a code-point fallback for runtimes without Segmenter. */
interface SegmenterLike {
  segment(input: string): Iterable<{ segment: string }>;
}
const graphemeSegmenter: SegmenterLike | null =
  typeof Intl !== "undefined" &&
  typeof (Intl as { Segmenter?: new (locale?: string, options?: { granularity?: string }) => SegmenterLike })
    .Segmenter === "function"
    ? new (Intl as { Segmenter: new (locale?: string, options?: { granularity?: string }) => SegmenterLike }).Segmenter(
        undefined,
        { granularity: "grapheme" },
      )
    : null;

function firstClusters(text: string, count: number): string[] {
  const out: string[] = [];
  if (count <= 0) return out;
  if (graphemeSegmenter) {
    for (const chunk of graphemeSegmenter.segment(text)) {
      out.push(chunk.segment);
      if (out.length >= count) break;
    }
    return out;
  }
  for (const cluster of Array.from(text)) {
    out.push(cluster);
    if (out.length >= count) break;
  }
  return out;
}

/** A token contributes initials when it starts with something letter-like or symbolic-readable. */
function meaningfulToken(token: string): boolean {
  if (!token) return false;
  const first = firstClusters(token, 1)[0] ?? "";
  if (!first) return false;
  // Punctuation-only tokens ("-", ".", '"') carry no identity.
  return /\p{L}|\p{N}|\p{Emoji_Presentation}|\p{Extended_Pictographic}/u.test(first);
}

/** Splits on whitespace plus common name/email separators. */
function tokenize(text: string): string[] {
  return text.split(/[\s._@\-+[\]()]+/u).filter(meaningfulToken);
}

function initialsFrom(name: string, email: string): string {
  const source = name.trim().length > 0 ? name : emailLocalPart(email);
  const tokens = tokenize(source);
  if (tokens.length === 0) return "?";
  if (tokens.length === 1) {
    return (firstClusters(tokens[0], 1)[0] ?? "?").toUpperCase();
  }
  const first = firstClusters(tokens[0], 1)[0] ?? "";
  const last = firstClusters(tokens[tokens.length - 1], 1)[0] ?? "";
  return `${first}${last}`.toUpperCase() || "?";
}

function emailLocalPart(email: string): string {
  const at = email.indexOf("@");
  const local = at > 0 ? email.slice(0, at) : email;
  return local.trim();
}

/**
 * Identity cache. Authors repeat across thousands of commits; deriving
 * initials walks Intl.Segmenter, so hits must stay cheap. Bounded FIFO: past
 * the cap the oldest key drops — re-derivation is always correct, merely not free.
 */
const IDENTITY_CACHE_CAP = 512;
const identityCache = new Map<string, AuthorIdentity>();

export function authorIdentity(name: string | null | undefined, email: string | null | undefined): AuthorIdentity {
  const safeName = typeof name === "string" ? name : "";
  const safeEmail = typeof email === "string" ? email : "";
  const trimmedName = safeName.trim().slice(0, 512);
  const trimmedEmail = safeEmail.trim().slice(0, 512);
  // Hue must be stable across display-name changes (key on email first), but
  // INITIALS follow the CURRENT name — caching by identity key alone served
  // renamed authors their old initials. The cache therefore spans both
  // inputs; renames simply miss once and re-derive.
  const cacheKey = `${trimmedEmail}\u0000${trimmedName}`;
  const cached = identityCache.get(cacheKey);
  if (cached) {
    // Refresh recency (Map iteration order = LRU order).
    identityCache.delete(cacheKey);
    identityCache.set(cacheKey, cached);
    return cached;
  }

  const key = trimmedEmail || trimmedName;
  const hash = fnv1a(key);
  const identity: AuthorIdentity = {
    key,
    hue: hueFromHash(hash),
    initials: initialsFrom(safeName, safeEmail),
  };
  identityCache.set(cacheKey, identity);
  if (identityCache.size > IDENTITY_CACHE_CAP) {
    const oldest = identityCache.keys().next();
    if (!oldest.done) identityCache.delete(oldest.value);
  }
  return identity;
}

/** Test hook: drop all cached identities. */
export function resetAuthorIdentityCache(): void {
  identityCache.clear();
}

/** Current cache size (test observability). */
export function authorIdentityCacheSize(): number {
  return identityCache.size;
}
