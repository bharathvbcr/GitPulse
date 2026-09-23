/**
 * Wire shapes for the Insights Secrets panel. Field names mirror the Rust
 * `SecretsReport` — no secret values are ever present on this type.
 */

export interface SecretFinding {
  rule_id: string;
  path: string;
  line: number;
  confidence: string;
  validation_outcome: string;
  fingerprint: string;
}

export interface SecretsReport {
  ok: boolean;
  error?: string | null;
  kingfisher_present: boolean;
  kingfisher_version?: string | null;
  nested_repos_scanned: boolean;
  findings: SecretFinding[];
  findings_truncated: boolean;
}

/** Narrow an IPC payload into a SecretsReport; refuse unknown shapes. */
export function parseSecretsReport(value: unknown): SecretsReport {
  if (!value || typeof value !== "object") {
    throw new Error("Secrets scan returned an unexpected payload");
  }
  const raw = value as Record<string, unknown>;
  const findingsRaw = Array.isArray(raw.findings) ? raw.findings : [];
  const findings: SecretFinding[] = [];
  for (const item of findingsRaw) {
    if (!item || typeof item !== "object") continue;
    const f = item as Record<string, unknown>;
    const rule_id = typeof f.rule_id === "string" ? f.rule_id : "";
    if (!rule_id) continue;
    findings.push({
      rule_id,
      path: typeof f.path === "string" ? f.path : "",
      line: typeof f.line === "number" && Number.isFinite(f.line) ? f.line : 0,
      confidence: typeof f.confidence === "string" ? f.confidence : "",
      validation_outcome:
        typeof f.validation_outcome === "string" ? f.validation_outcome : "",
      fingerprint: typeof f.fingerprint === "string" ? f.fingerprint : "",
    });
  }
  return {
    ok: raw.ok === true,
    error: typeof raw.error === "string" ? raw.error : null,
    kingfisher_present: raw.kingfisher_present === true,
    kingfisher_version:
      typeof raw.kingfisher_version === "string" ? raw.kingfisher_version : null,
    nested_repos_scanned: raw.nested_repos_scanned !== false,
    findings,
    findings_truncated: raw.findings_truncated === true,
  };
}
