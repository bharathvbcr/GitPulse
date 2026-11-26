import { beforeEach, describe, expect, it } from "vitest";
import { render } from "svelte/server";
import ManviStewardSettings from "./ManviStewardSettings.svelte";
import {
  DEFAULT_MANVI_STEWARD_SETTINGS,
  EMPTY_MANVI_STEWARD_STATUS,
  resolveEffectiveStewardPolicy,
  setManviStewardForTest,
} from "../stores/stewardStore";
import {
  BANNED_UI_TERMS,
  RETIRED_VERB_TERMS,
  VOICE_TERMS,
  findBannedTerms,
} from "../ui/copy/vocabulary";

function stripHtmlTags(markup: string): string {
  return markup
    .replace(/<!--[\s\S]*?-->/g, " ")
    .replace(/<[^>]+>/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

describe("ManviStewardSettings", () => {
  beforeEach(() => {
    setManviStewardForTest(EMPTY_MANVI_STEWARD_STATUS);
  });

  it("renders unconfigured Manvi guidance and clean UI copy when no model is chosen", () => {
    const { body } = render(ManviStewardSettings, {
      props: { active: false, repoPath: null },
    });

    expect(body).toContain("Manvi system steward");
    expect(body).toContain('id="manvi-section-steward"');
    expect(body).toContain("Manvi model not configured yet");
    expect(body).toContain("Choose Manvi model");
    expect(body).toContain("preserving Cargo registry downloads");
    expect(body).toContain("finished system-temp session scratchpads");
    expect(body).toContain("evaluates the candidate inventory and selects idle items to reclaim");
    expect(body).toContain("recent idle scratch builds when disk space is critical");
    expect(body).not.toContain("shared Cargo or Go caches");
    expect(body).not.toContain('data-testid="manvi-steward-repo-scope"');

    const visibleText = stripHtmlTags(body);
    expect(findBannedTerms(visibleText, BANNED_UI_TERMS)).toEqual([]);
    expect(findBannedTerms(visibleText, RETIRED_VERB_TERMS)).toEqual([]);
    expect(findBannedTerms(visibleText, VOICE_TERMS)).toEqual([]);
  });

  it("renders cloud/local Manvi readiness, per-repository overrides, critical banner, and latest check", () => {
    const commonDir = "/Users/dev/work/app/.git";
    const settings = {
      ...DEFAULT_MANVI_STEWARD_SETTINGS,
      interval_secs: 300,
      clean_retention_days: 14,
      repositories: {
        [commonDir]: {
          enabled: true,
          message_agents: true,
          slow_critical: false,
          clean_critical: true,
        },
      },
    };
    setManviStewardForTest({
      settings,
      effective: resolveEffectiveStewardPolicy(settings, commonDir, null),
      repo_common_dir: commonDir,
      env_override: null,
      env_forced: null,
      manvi_state: "ready",
      manvi_mode: "local",
      manvi_model: "qwen2.5-coder:14b",
      manvi_detail: "Local model via Manvi",
      running: false,
      critical_throttle_active: true,
      throttled_count: 3,
      last_check_ms: 1_763_550_000_000,
      next_check_ms: 1_763_550_300_000,
      history: [
        {
          id: "steward-1",
          trigger: "critical_wake",
          ran_at_ms: 1_763_550_000_000,
          outcome: "acted",
          tier: "severe",
          binding: "low_disk",
          manvi_mode: "local",
          manvi_model: "qwen2.5-coder:14b",
          model_generated: true,
          summary: "Host pressure is severe (low disk); reclaimed build caches and messaged sessions.",
          directive: "Pause heavy builds and cap Cargo concurrency until disk headroom recovers.",
          messaged_sessions: 2,
          messaged_repositories: 1,
          skipped_untrusted_repositories: 0,
          throttled_processes: 3,
          restored_processes: 0,
          reclaimed_bytes: Math.round(2.5 * 1024 * 1024 * 1024),
          cleaned_items: [
            {
              repo_path: "/Users/dev/work/app",
              target: "scratch:.gitpulse/scratch/target-ci",
              status: "reclaimed",
              measured_bytes: 512 * 1024 * 1024,
              bytes_after: 0,
              detail: "Selected by Manvi cloud; removed idle agent Cargo scratch build.",
            },
          ],
          warnings: [
            "Skipped calling Manvi's local model server during severe memory pressure.",
            "/Users/dev/my_repo: token ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 failed",
          ],
        },
      ],
    });

    const { body } = render(ManviStewardSettings, {
      props: { active: false, repoPath: "/Users/dev/work/app" },
    });

    expect(body).toContain("Manvi active (local · qwen2.5-coder:14b)");
    expect(body).toContain("Critical slowdown active · 3 background agent worker processes throttled");
    expect(body).toContain('data-testid="manvi-steward-repo-scope"');
    expect(body).toContain("Enable steward in this repository");
    expect(body).toContain("Reclaimed 2.5 GiB");
    expect(body).toContain('data-testid="manvi-steward-cleaned-items"');
    expect(body).toContain("scratch:.gitpulse/scratch/target-ci");
    expect(body).toContain("Selected by Manvi cloud");
    expect(body).toContain("Pause heavy builds and cap Cargo concurrency");
    expect(body).toContain("low disk");
    expect(body).toContain("Skipped calling Manvi");
    // Paths keep underscores intact while secret prefixes are still redacted.
    expect(body).toContain("/Users/dev/my_repo");
    expect(body).not.toContain("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789");
  });

  it("messages agents by default and leaves cleaning and slowing for the person to switch on", () => {
    expect(DEFAULT_MANVI_STEWARD_SETTINGS.enabled).toBe(true);
    expect(DEFAULT_MANVI_STEWARD_SETTINGS.message_agents).toBe(true);
    expect(DEFAULT_MANVI_STEWARD_SETTINGS.clean_critical).toBe(false);
    expect(DEFAULT_MANVI_STEWARD_SETTINGS.slow_critical).toBe(false);
  });

  it("keeps three days of build caches by default and selects the 3d choice", () => {
    expect(DEFAULT_MANVI_STEWARD_SETTINGS.clean_retention_days).toBe(3);
    const { body } = render(ManviStewardSettings, {
      props: { active: false, repoPath: null },
    });
    const retention = body.slice(body.indexOf('data-testid="manvi-steward-retention"'));
    const pressed = [...retention.matchAll(/aria-pressed="true"[^>]*>\s*([^<]+?)\s*</g)].map((m) => m[1]);
    expect(pressed[0]).toBe("3d");
    for (const label of ["3d", "7d", "14d", "30d"]) expect(retention).toContain(`>${label}<`);
  });

  it("shows a hand-edited interval or retention as what is in force, not a preset", () => {
    const settings = { ...DEFAULT_MANVI_STEWARD_SETTINGS, interval_secs: 240, clean_retention_days: 5 };
    setManviStewardForTest({
      ...EMPTY_MANVI_STEWARD_STATUS,
      settings,
      effective: resolveEffectiveStewardPolicy(settings, null, null),
    });
    const { body } = render(ManviStewardSettings, { props: { active: false, repoPath: null } });
    const pressedLabels = [...body.matchAll(/aria-pressed="true"[^>]*>\s*([^<]+?)\s*</g)].map((m) => m[1]);
    expect(pressedLabels).toContain("4m");
    expect(pressedLabels).toContain("5d");
    expect(pressedLabels).not.toContain("3m");
    expect(pressedLabels).not.toContain("3d");
  });

  it("says Anthropic rejected the key instead of calling the model active or unchecked", () => {
    const detail =
      "Anthropic rejected the API key Manvi used: it is wrong, revoked or disabled. Replace the Claude key in GitPulse settings, or fix the account in ~/.anthropic-accounts when you use one.";
    setManviStewardForTest({
      ...EMPTY_MANVI_STEWARD_STATUS,
      manvi_state: "rejected",
      manvi_mode: "cloud",
      manvi_model: "claude-sonnet-5-5",
      manvi_detail: detail,
    });
    const { body } = render(ManviStewardSettings, {
      props: { active: false, repoPath: null },
    });
    expect(body).toContain('data-testid="manvi-steward-model-rejected"');
    expect(body).toContain("Anthropic rejected the API key");
    expect(body).not.toContain('data-testid="manvi-steward-model-ready"');
    expect(body).not.toContain('data-testid="manvi-steward-model-unknown"');
    const visibleText = stripHtmlTags(body);
    expect(findBannedTerms(visibleText, BANNED_UI_TERMS)).toEqual([]);
    expect(findBannedTerms(visibleText, RETIRED_VERB_TERMS)).toEqual([]);
    expect(findBannedTerms(visibleText, VOICE_TERMS)).toEqual([]);
  });

  it("distinguishes deferred or failed Manvi checks from an unchosen model", () => {
    setManviStewardForTest({
      ...EMPTY_MANVI_STEWARD_STATUS,
      manvi_state: "deferred",
      manvi_detail: "Host load deferred checking Manvi.",
    });
    const { body } = render(ManviStewardSettings, {
      props: { active: false, repoPath: null },
    });
    expect(body).toContain('data-testid="manvi-steward-model-unknown"');
    expect(body).not.toContain('data-testid="manvi-steward-choose-model"');

    const visibleText = stripHtmlTags(body);
    expect(findBannedTerms(visibleText, BANNED_UI_TERMS)).toEqual([]);
    expect(findBannedTerms(visibleText, RETIRED_VERB_TERMS)).toEqual([]);
    expect(findBannedTerms(visibleText, VOICE_TERMS)).toEqual([]);

    setManviStewardForTest({
      ...EMPTY_MANVI_STEWARD_STATUS,
      settings: { ...DEFAULT_MANVI_STEWARD_SETTINGS, enabled: false },
      manvi_state: "unchecked",
      manvi_detail: "Manvi model wasn't checked because the system steward is switched off.",
    });
    const uncheckedRender = render(ManviStewardSettings, {
      props: { active: false, repoPath: null },
    });
    expect(uncheckedRender.body).toContain("Manvi model not checked while steward is off");
    expect(uncheckedRender.body).not.toContain('data-testid="manvi-steward-choose-model"');
    const uncheckedText = stripHtmlTags(uncheckedRender.body);
    expect(findBannedTerms(uncheckedText, BANNED_UI_TERMS)).toEqual([]);
    expect(findBannedTerms(uncheckedText, RETIRED_VERB_TERMS)).toEqual([]);
    expect(findBannedTerms(uncheckedText, VOICE_TERMS)).toEqual([]);
  });

  it("resolves global defaults, repository overrides, and Rust lowercase environment overrides consistently", () => {
    const commonDir = "/repo/.git";
    const base = {
      ...DEFAULT_MANVI_STEWARD_SETTINGS,
      enabled: true,
      slow_critical: true,
      clean_critical: true,
      repositories: {
        [commonDir]: {
          slow_critical: false,
        },
      },
    };

    const forRepo = resolveEffectiveStewardPolicy(base, commonDir, null);
    expect(forRepo.overridden).toBe(true);
    expect(forRepo.enabled).toBe(true);
    expect(forRepo.slow_critical).toBe(false);
    expect(forRepo.clean_critical).toBe(true);

    const otherRepo = resolveEffectiveStewardPolicy(base, "/other/.git", null);
    expect(otherRepo.overridden).toBe(false);
    expect(otherRepo.slow_critical).toBe(true);

    const envForcedOff = resolveEffectiveStewardPolicy(
      base,
      commonDir,
      false,
    );
    expect(envForcedOff.enabled).toBe(false);

    const envForcedOn = resolveEffectiveStewardPolicy(
      { ...base, enabled: false },
      commonDir,
      true,
    );
    expect(envForcedOn.enabled).toBe(true);
  });
});
