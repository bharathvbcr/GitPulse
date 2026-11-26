//! Recall of the commit secret scan, measured against Kingfisher and
//! Betterleaks on one corpus.
//!
//! The corpus is one plant per Kingfisher rule, sampled from the rule's own
//! regex (`plant_sampler`) and kept only when Kingfisher itself reports that
//! rule on it, so Kingfisher scores 100% by construction and the table says
//! so. Betterleaks and dc-redact (through [`Tally`], the path the commit gate
//! runs) are then scored on the same files.
//!
//! Some rules are reported only when one of Kingfisher's hidden helper rules
//! matches the same file (a client secret beside its client id). A rule
//! confirmed on no candidate alone is planted again after samples of the
//! hidden helpers sharing its vendor stem and scanned a second time. Those
//! plants are reported in their own table, never in the comparable one, and
//! a detector is credited on them only for a hit on the secret's lines.
//!
//! Nothing here is a token. Plants are produced at run time into a temporary
//! directory that is deleted afterwards, the oracles run with `--redact`, and
//! the report names rules, never values; [`assert_no_plant_in`] checks that
//! before the report is printed or written.
//!
//! Run (also in `docs/PERFORMANCE.md`):
//!
//! ```text
//! GITPULSE_LEAK_RECALL_OUT=/tmp/recall.md cargo test --manifest-path src-tauri/Cargo.toml \
//!   --lib leak_scan::recall_tests::measure_recall -- --ignored --exact --nocapture
//! ```
//!
//! `GITPULSE_KINGFISHER` / `GITPULSE_BETTERLEAKS` name the binaries when they
//! are not on PATH. A missing oracle, an unexpected exit, an unreadable report
//! or an oracle that finds nothing in a non-empty corpus fails the run; none
//! of them is ever scored as zero.

use super::corpus_tests::Seeded;
use super::plant_sampler::{self, Length};
use super::{Tally, ENGINE};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Candidates sampled per rule at each length; the first one Kingfisher
/// confirms is the rule's plant, minimum-length candidates first.
const MIN_CANDIDATES: usize = 4;
const REALISTIC_CANDIDATES: usize = 2;
/// Draws per wanted candidate before the rule is reported as unsampleable.
const SAMPLE_ATTEMPTS: u64 = 64;
/// Ceiling on one oracle process.
const ORACLE_DEADLINE: Duration = Duration::from_secs(15 * 60);
/// Betterleaks' exit status for "findings"; distinct from its error status.
const BETTERLEAKS_FINDINGS_EXIT: i32 = 42;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "high" => Ok(Self::High),
            "medium" => Ok(Self::Medium),
            "low" => Ok(Self::Low),
            other => Err(format!("unknown rule confidence `{other}`")),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }
}

#[derive(Debug, Clone)]
struct Rule {
    id: String,
    confidence: Confidence,
    pattern: String,
}

/// One candidate file: the rule's sampled secret, after the samples of the
/// hidden helper rules it is planted beside (none for a plain candidate).
#[derive(Debug, Clone)]
struct Candidate {
    rule: usize,
    length: Length,
    /// Helper samples, each ending in a newline; empty for a plain candidate.
    prefix: String,
    secret: String,
    /// Indices into the hidden helper rules sampled into `prefix`.
    helpers: Vec<usize>,
}

impl Candidate {
    fn plain(rule: usize, length: Length, secret: String) -> Self {
        Self {
            rule,
            length,
            prefix: String::new(),
            secret,
            helpers: Vec::new(),
        }
    }

    fn file_name(&self, index: usize) -> String {
        let kind = if self.helpers.is_empty() { 'c' } else { 'p' };
        format!(
            "{kind}{index:05}-r{:04}-{}.txt",
            self.rule,
            self.length.label()
        )
    }

    fn text(&self) -> String {
        format!("{}{}", self.prefix, self.secret)
    }

    fn secret_line(&self) -> u32 {
        secret_line_after(&self.prefix)
    }
}

/// The 1-based line a secret starts on when `prefix` precedes it. Counted
/// from the prefix's newlines, not its helpers: a helper sample can span
/// lines (`(?:.|[\n\r])` in several helper patterns).
fn secret_line_after(prefix: &str) -> u32 {
    1 + u32::try_from(prefix.matches('\n').count()).unwrap_or(u32::MAX - 1)
}

/// Where a detector's hits in one file fell, relative to the secret's first
/// line: on the secret (that line or later), or only on helper lines before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Placed {
    secret: bool,
    helper_only: bool,
}

fn place(hit_lines: impl IntoIterator<Item = u32>, secret_line: u32) -> Placed {
    let (mut secret, mut helper) = (false, false);
    for line in hit_lines {
        if line >= secret_line {
            secret = true;
        } else {
            helper = true;
        }
    }
    Placed {
        secret,
        helper_only: helper && !secret,
    }
}

/// Why a rule has no confirmed plant.
#[derive(Debug, Clone)]
enum NoPlant {
    Unsupported(String),
    NoMatchingSample,
    NotConfirmed {
        kingfisher_reported: BTreeSet<String>,
    },
}

// ---------------------------------------------------------------- oracles --

/// Find an oracle binary: `env_var` when set (it must name a file), else
/// PATH and the GUI-launch fallback directories, as the app resolves tools.
fn resolve_oracle(
    name: &str,
    env_var: &str,
    override_path: Option<OsString>,
    find: impl Fn(&str) -> Option<String>,
) -> Result<PathBuf, String> {
    if let Some(path) = override_path {
        let path = PathBuf::from(path);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!(
                "{env_var} names {}, which is not a file; a missing oracle is never scored as zero",
                path.display()
            ))
        };
    }
    find(name).map(PathBuf::from).ok_or_else(|| {
        format!(
            "oracle `{name}` is not installed (not on PATH or the fallback directories; \
             set {env_var}); a missing oracle is never scored as zero"
        )
    })
}

fn oracle(name: &str, env_var: &str) -> Result<PathBuf, String> {
    resolve_oracle(name, env_var, std::env::var_os(env_var), |n| {
        crate::engine::process::find_external_tool(n)
    })
}

struct Ran {
    status: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Run `bin` with only PATH and HOME from this process (the same scrubbed
/// environment the app gives Kingfisher), output to files, under a deadline.
fn run(bin: &Path, args: &[&str], scratch: &Path, tag: &str) -> Result<Ran, String> {
    let out_path = scratch.join(format!("{tag}.stdout"));
    let err_path = scratch.join(format!("{tag}.stderr"));
    let open = |p: &Path| std::fs::File::create(p).map_err(|e| format!("{}: {e}", p.display()));
    let env = crate::secrets::build_scrubbed_env(
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("HOME").as_deref(),
    );
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(scratch)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(open(&out_path)?)
        .stderr(open(&err_path)?);
    let (mut child, registration) = crate::procguard::spawn(&mut cmd, "leak-scan recall oracle")
        .map_err(|e| format!("could not start {}: {e}", bin.display()))?;
    let started = Instant::now();
    let status = loop {
        match registration.poll(|| child.try_wait()) {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > ORACLE_DEADLINE => {
                // The child and its process group are ours; reap before failing.
                let _ = registration.stop_and_reap(&mut child, Duration::from_secs(2));
                return Err(format!(
                    "{} {tag} ran past {ORACLE_DEADLINE:?}",
                    bin.display()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("waiting for {}: {e}", bin.display())),
        }
    };
    let read = |p: &Path| std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()));
    Ok(Ran {
        status: status
            .code()
            .ok_or_else(|| format!("{} {tag} ended by a signal", bin.display()))?,
        stdout: read(&out_path)?,
        stderr: read(&err_path)?,
    })
}

fn failure(bin: &Path, tag: &str, ran: &Ran) -> String {
    let stderr = String::from_utf8_lossy(&ran.stderr);
    let tail: String = stderr.chars().take(600).collect();
    format!(
        "{} {tag} exited {}: {}",
        bin.display(),
        ran.status,
        redact_runs(&tail)
    )
}

/// Replace every run of 16 or more token characters with a marker, so an
/// oracle's stderr cannot carry a plant into a panic message.
fn redact_runs(text: &str) -> String {
    let mut out = String::new();
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.len() >= 16 {
            out.push_str("[run redacted]");
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in text.chars() {
        if is_token_char(c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "+/=_-.".contains(c)
}

fn first_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// The file names in a directory listing's paths, which the oracles may
/// report canonicalised (`/private/tmp` for `/tmp`).
fn file_name_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Kingfisher's visible rules, and the ones it hides (helper rules, never
/// reported on their own).
fn kingfisher_rules(bin: &Path, scratch: &Path) -> Result<(Vec<Rule>, Vec<Rule>), String> {
    let ran = run(
        bin,
        &["rules", "list", "--format", "json"],
        scratch,
        "kf-rules",
    )?;
    if ran.status != 0 {
        return Err(failure(bin, "rules list", &ran));
    }
    let rows: Vec<Value> = serde_json::from_slice(&ran.stdout)
        .map_err(|e| format!("kingfisher rules list is not a JSON array: {e}"))?;
    let mut rules = Vec::new();
    let mut hidden = Vec::new();
    for row in &rows {
        let field = |k: &str| {
            row.get(k)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("kingfisher rule without `{k}`"))
        };
        let rule = Rule {
            id: field("id")?.to_string(),
            confidence: Confidence::parse(field("confidence")?)?,
            pattern: field("pattern")?.to_string(),
        };
        if row.get("visible").and_then(Value::as_bool) == Some(false) {
            hidden.push(rule);
        } else {
            rules.push(rule);
        }
    }
    if rules.is_empty() {
        return Err("kingfisher listed no rules".into());
    }
    Ok((rules, hidden))
}

/// One rule's findings in one file: the confidence Kingfisher gave the first,
/// and the 1-based lines of all of them.
#[derive(Debug, Clone)]
struct KfFinding {
    confidence: Confidence,
    lines: BTreeSet<u32>,
}

/// file name -> rule id -> its findings there.
type KingfisherFindings = HashMap<String, BTreeMap<String, KfFinding>>;

/// A finding's 1-based line, which both oracles report as a JSON number.
fn line_of(v: Option<&Value>) -> Option<u32> {
    v.and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

fn kingfisher_scan(
    bin: &Path,
    dir: &Path,
    scratch: &Path,
    tag: &str,
) -> Result<KingfisherFindings, String> {
    let report = scratch.join(format!("{tag}.json"));
    let dir_arg = dir.to_string_lossy();
    let report_arg = report.to_string_lossy();
    let ran = run(
        bin,
        &[
            "scan",
            &dir_arg,
            "--no-validate",
            "--no-dedup",
            "--redact",
            "--confidence",
            "low",
            "--format",
            "json",
            "--output",
            &report_arg,
            "--git-history",
            "none",
            "--no-update-check",
            "--quiet",
        ],
        scratch,
        tag,
    )?;
    if !crate::secrets::SUCCESS_EXITS.contains(&ran.status) {
        return Err(failure(bin, "scan", &ran));
    }
    let body: Value = serde_json::from_slice(
        &std::fs::read(&report).map_err(|e| format!("kingfisher wrote no report: {e}"))?,
    )
    .map_err(|e| format!("kingfisher report is not JSON: {e}"))?;
    let findings = body
        .get("findings")
        .and_then(Value::as_array)
        .ok_or("kingfisher report has no `findings` array")?;
    let mut by_file: KingfisherFindings = HashMap::new();
    for f in findings {
        let rule = f.pointer("/rule/id").and_then(Value::as_str);
        let path = f.pointer("/finding/path").and_then(Value::as_str);
        let confidence = f.pointer("/finding/confidence").and_then(Value::as_str);
        let line = line_of(f.pointer("/finding/line"));
        let (Some(rule), Some(path), Some(confidence), Some(line)) = (rule, path, confidence, line)
        else {
            return Err("kingfisher finding without rule id, path, confidence or line".into());
        };
        let confidence = Confidence::parse(confidence)?;
        by_file
            .entry(file_name_of(path))
            .or_default()
            .entry(rule.to_string())
            .or_insert_with(|| KfFinding {
                confidence,
                lines: BTreeSet::new(),
            })
            .lines
            .insert(line);
    }
    Ok(by_file)
}

/// Betterleaks' rule ids, from its resolved default config.
fn betterleaks_rule_ids(bin: &Path, scratch: &Path) -> Result<BTreeSet<String>, String> {
    let ran = run(bin, &["config", "show"], scratch, "bl-config")?;
    if ran.status != 0 {
        return Err(failure(bin, "config show", &ran));
    }
    let ids: BTreeSet<String> = String::from_utf8_lossy(&ran.stdout)
        .lines()
        .filter_map(|l| l.trim().strip_prefix("id = "))
        .map(|v| v.trim_matches(|c| c == '\'' || c == '"').to_string())
        .collect();
    if ids.is_empty() {
        return Err("betterleaks config show listed no rule ids".into());
    }
    Ok(ids)
}

/// file name -> the 1-based start lines of Betterleaks' findings in it.
fn betterleaks_scan(
    bin: &Path,
    dir: &Path,
    scratch: &Path,
) -> Result<HashMap<String, Vec<u32>>, String> {
    // Betterleaks exits 0 on a directory that does not exist.
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    let report = scratch.join("bl-scan.json");
    let dir_arg = dir.to_string_lossy();
    let report_arg = report.to_string_lossy();
    let exit_arg = BETTERLEAKS_FINDINGS_EXIT.to_string();
    let ran = run(
        bin,
        &[
            "dir",
            &dir_arg,
            "--no-banner",
            "--no-color",
            "--redact",
            "--confidence",
            "low",
            "--report-format",
            "json",
            "--report-path",
            &report_arg,
            "--log-level",
            "error",
            "--exit-code",
            &exit_arg,
        ],
        scratch,
        "bl-scan",
    )?;
    if ran.status != 0 && ran.status != BETTERLEAKS_FINDINGS_EXIT {
        return Err(failure(bin, "dir", &ran));
    }
    let rows: Vec<Value> = serde_json::from_slice(
        &std::fs::read(&report).map_err(|e| format!("betterleaks wrote no report: {e}"))?,
    )
    .map_err(|e| format!("betterleaks report is not a JSON array: {e}"))?;
    let mut by_file: HashMap<String, Vec<u32>> = HashMap::new();
    for row in &rows {
        let rule = row.get("RuleID").and_then(Value::as_str);
        let path = row.get("File").and_then(Value::as_str);
        let line = line_of(row.get("StartLine"));
        let (Some(_), Some(path), Some(line)) = (rule, path, line) else {
            return Err("betterleaks finding without RuleID, File or StartLine".into());
        };
        by_file.entry(file_name_of(path)).or_default().push(line);
    }
    Ok(by_file)
}

// ---------------------------------------------------------------- corpus --

/// FNV-1a, so a rule's plants depend on its id and not on its position in
/// Kingfisher's listing.
fn stable_seed(id: &str) -> u64 {
    id.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// `GITPULSE_LEAK_RECALL_SALT`: a different, still deterministic corpus, for
/// measuring how much the numbers move with the sampler's choices. 0 (unset)
/// is the published corpus.
fn corpus_variant() -> Result<u64, String> {
    match std::env::var("GITPULSE_LEAK_RECALL_SALT") {
        Err(std::env::VarError::NotPresent) => Ok(0),
        Ok(v) => v
            .parse()
            .map_err(|_| format!("GITPULSE_LEAK_RECALL_SALT is not a number: {v:?}")),
        Err(e) => Err(format!("GITPULSE_LEAK_RECALL_SALT: {e}")),
    }
}

/// Sample a rule's candidates, or say why it has none.
fn sample_rule(
    index: usize,
    rule: &Rule,
    corpus_variant: u64,
) -> Result<(Vec<Candidate>, bool), NoPlant> {
    let pattern = plant_sampler::parse(&rule.pattern).map_err(NoPlant::Unsupported)?;
    // A pattern the regex crate rejects is still sampled; Kingfisher decides.
    let check = regex::Regex::new(&rule.pattern).ok();
    let validated = check.is_some();
    let mut out: Vec<Candidate> = Vec::new();
    let base = stable_seed(&rule.id) ^ corpus_variant.wrapping_mul(0xD6E8_FEB8_6659_FD93);
    for (length, wanted) in [
        (Length::Min, MIN_CANDIDATES),
        (Length::Realistic, REALISTIC_CANDIDATES),
    ] {
        let mut got = 0;
        let budget = SAMPLE_ATTEMPTS * wanted as u64;
        for attempt in 0..budget {
            if got == wanted {
                break;
            }
            let seed = base ^ (attempt + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ length as u64;
            let text = plant_sampler::sample(&pattern, &mut Seeded(seed), length);
            let matches = check.as_ref().is_none_or(|re| re.is_match(&text));
            if text.trim().is_empty() || !matches || out.iter().any(|c| c.secret == text) {
                continue;
            }
            out.push(Candidate::plain(index, length, text));
            got += 1;
        }
    }
    if out.is_empty() {
        return Err(NoPlant::NoMatchingSample);
    }
    Ok((out, validated))
}

fn write_file(dir: &Path, name: &str, text: &str) -> Result<(), String> {
    let path = dir.join(name);
    std::fs::write(&path, format!("{text}\n")).map_err(|e| format!("{}: {e}", path.display()))
}

/// dc-redact through the commit gate's own entry point. The shape it named,
/// when it found one.
fn dc_redact(name: &str, text: &str) -> Option<&'static str> {
    dc_redact_hits(name, text).first().map(|(_, shape)| *shape)
}

/// Every line dc-redact flags in `text`, with the shape it named there.
fn dc_redact_hits(name: &str, text: &str) -> Vec<(u32, &'static str)> {
    let mut tally = Tally::default();
    tally.scan_blob(name, format!("{text}\n").as_bytes());
    tally.hits.iter().map(|h| (h.line, h.rule)).collect()
}

/// dc-redact on a candidate file: the shape it named on the secret's lines,
/// and whether it flagged only helper lines.
fn dc_redact_placed(name: &str, c: &Candidate) -> (Option<&'static str>, bool) {
    let hits = dc_redact_hits(name, &c.text());
    let line = c.secret_line();
    let placed = place(hits.iter().map(|(l, _)| *l), line);
    let shape = hits.iter().find(|(l, _)| *l >= line).map(|(_, s)| *s);
    (shape, placed.helper_only)
}

// ---------------------------------------------------------------- vendors --

/// Ranking tiers for dc-redact's misses: how likely a leaked credential of
/// this kind is to be committed and to matter. Cloud, CI and package
/// registries, payments, AI providers and databases first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    Cloud,
    CiAndPackages,
    Payments,
    AiProviders,
    Databases,
    Messaging,
    Observability,
    IdentityAndSecrets,
    Generic,
    Other,
}

impl Tier {
    fn label(self) -> &'static str {
        match self {
            Self::Cloud => "cloud",
            Self::CiAndPackages => "CI, source and packages",
            Self::Payments => "payments and finance",
            Self::AiProviders => "AI providers",
            Self::Databases => "databases and data",
            Self::Messaging => "messaging and email",
            Self::Observability => "observability and on-call",
            Self::IdentityAndSecrets => "identity and secret managers",
            Self::Generic => "generic and context",
            Self::Other => "other SaaS",
        }
    }
}

/// (vendor, tier, rule-id stems it covers). A stem matches a rule id's first
/// `-`-separated segment, or a prefix of Veles' glued ids.
const VENDORS: &[(&str, Tier, &[&str])] = &[
    (
        "aws",
        Tier::Cloud,
        &["aws", "codecommitcredentials", "codecatalystcredentials"],
    ),
    ("google cloud", Tier::Cloud, &["gcp", "google"]),
    ("azure", Tier::Cloud, &["azure", "microsoft"]),
    (
        "cloudflare",
        Tier::Cloud,
        &["cloudflare", "cloudflareapitoken"],
    ),
    (
        "digitalocean",
        Tier::Cloud,
        &["digitalocean", "digitaloceanapikey"],
    ),
    ("heroku", Tier::Cloud, &["heroku", "herokuplatformkey"]),
    ("vercel", Tier::Cloud, &["vercel"]),
    ("netlify", Tier::Cloud, &["netlify"]),
    ("alibaba", Tier::Cloud, &["alibaba"]),
    ("fly.io", Tier::Cloud, &["flyio"]),
    ("ibm", Tier::Cloud, &["ibm"]),
    ("ovh", Tier::Cloud, &["ovh"]),
    ("scaleway", Tier::Cloud, &["scaleway"]),
    ("scalingo", Tier::Cloud, &["scalingo"]),
    ("upcloud", Tier::Cloud, &["upcloud"]),
    ("vultr", Tier::Cloud, &["vultr"]),
    ("civo", Tier::Cloud, &["civo"]),
    ("exoscale", Tier::Cloud, &["exoscale"]),
    ("openshift", Tier::Cloud, &["openshift"]),
    ("kubernetes", Tier::Cloud, &["kubernetes"]),
    ("render", Tier::Cloud, &["render"]),
    ("yandex", Tier::Cloud, &["yandex"]),
    ("infomaniak", Tier::Cloud, &["infomaniak"]),
    ("runpod", Tier::Cloud, &["runpod"]),
    ("fastly", Tier::Cloud, &["fastly"]),
    ("github", Tier::CiAndPackages, &["github"]),
    ("gitlab", Tier::CiAndPackages, &["gitlab"]),
    (
        "bitbucket",
        Tier::CiAndPackages,
        &["bitbucket", "bitbucketcredentials"],
    ),
    ("npm", Tier::CiAndPackages, &["npm", "npmjsaccesstoken"]),
    ("pypi", Tier::CiAndPackages, &["pypi"]),
    ("docker", Tier::CiAndPackages, &["docker", "dockerhub"]),
    ("rubygems", Tier::CiAndPackages, &["rubygems"]),
    ("nuget", Tier::CiAndPackages, &["nuget"]),
    ("crates.io", Tier::CiAndPackages, &["crates"]),
    ("clojars", Tier::CiAndPackages, &["clojars"]),
    (
        "packagist",
        Tier::CiAndPackages,
        &[
            "packagistorgreadtoken",
            "packagistorgupdatetoken",
            "packagistsecret",
        ],
    ),
    ("gitea", Tier::CiAndPackages, &["gitea"]),
    ("artifactory", Tier::CiAndPackages, &["artifactory"]),
    ("cloudsmith", Tier::CiAndPackages, &["cloudsmith"]),
    (
        "circleci",
        Tier::CiAndPackages,
        &["circleci", "circleciproject"],
    ),
    ("travis ci", Tier::CiAndPackages, &["travisci"]),
    ("drone ci", Tier::CiAndPackages, &["droneci"]),
    ("buildkite", Tier::CiAndPackages, &["buildkite"]),
    ("bitrise", Tier::CiAndPackages, &["bitrise"]),
    ("harness", Tier::CiAndPackages, &["harness"]),
    (
        "hashicorp",
        Tier::CiAndPackages,
        &["hashicorp", "vault", "hcpclientcredentials"],
    ),
    ("pulumi", Tier::CiAndPackages, &["pulumi"]),
    ("scalr", Tier::CiAndPackages, &["scalr"]),
    ("octopus deploy", Tier::CiAndPackages, &["octopus"]),
    ("codecov", Tier::CiAndPackages, &["codecov"]),
    ("coveralls", Tier::CiAndPackages, &["coveralls"]),
    ("sonar", Tier::CiAndPackages, &["sonar"]),
    ("snyk", Tier::CiAndPackages, &["snyk"]),
    ("mergify", Tier::CiAndPackages, &["mergify"]),
    ("infracost", Tier::CiAndPackages, &["infracost"]),
    ("prefect", Tier::CiAndPackages, &["prefect"]),
    ("sourcegraph", Tier::CiAndPackages, &["sourcegraph"]),
    ("endor labs", Tier::CiAndPackages, &["endorlabs"]),
    ("deno", Tier::CiAndPackages, &["deno", "denopatorg"]),
    (
        "postman",
        Tier::CiAndPackages,
        &["postman", "postmanapikey", "postmancollectiontoken"],
    ),
    ("ngrok", Tier::CiAndPackages, &["ngrok"]),
    ("tailscale", Tier::CiAndPackages, &["tailscale"]),
    ("launchdarkly", Tier::CiAndPackages, &["launchdarkly"]),
    ("configcat", Tier::CiAndPackages, &["configcat"]),
    ("devcycle", Tier::CiAndPackages, &["devcycle"]),
    ("ionic", Tier::CiAndPackages, &["ionic"]),
    ("sidekiq", Tier::CiAndPackages, &["sidekiq"]),
    ("temporal", Tier::CiAndPackages, &["temporal"]),
    ("thunderstore", Tier::CiAndPackages, &["thunderstore"]),
    ("stripe", Tier::Payments, &["stripe"]),
    ("square", Tier::Payments, &["square"]),
    ("paypal", Tier::Payments, &["paypal"]),
    ("plaid", Tier::Payments, &["plaid"]),
    ("shopify", Tier::Payments, &["shopify"]),
    ("coinbase", Tier::Payments, &["coinbase"]),
    ("flutterwave", Tier::Payments, &["flutterwave"]),
    ("razorpay", Tier::Payments, &["razorpay"]),
    ("paddle", Tier::Payments, &["paddle"]),
    ("gocardless", Tier::Payments, &["gocardless"]),
    ("checkout.com", Tier::Payments, &["checkout"]),
    ("xendit", Tier::Payments, &["xendit"]),
    ("midtrans", Tier::Payments, &["midtrans"]),
    ("asaas", Tier::Payments, &["asaas"]),
    ("finicity", Tier::Payments, &["finicity"]),
    ("mercury", Tier::Payments, &["mercury"]),
    ("ramp", Tier::Payments, &["ramp"]),
    ("polar", Tier::Payments, &["polar"]),
    ("gumroad", Tier::Payments, &["gumroad"]),
    ("freemius", Tier::Payments, &["freemius"]),
    ("kraken", Tier::Payments, &["kraken"]),
    ("kucoin", Tier::Payments, &["kucoin"]),
    ("bittrex", Tier::Payments, &["bittrex"]),
    ("highnote", Tier::Payments, &["highnote"]),
    ("woocommerce", Tier::Payments, &["woocommerce"]),
    ("squarespace", Tier::Payments, &["squarespace"]),
    ("freshbooks", Tier::Payments, &["freshbooks"]),
    ("duffel", Tier::Payments, &["duffel"]),
    ("easypost", Tier::Payments, &["easypost"]),
    ("shippo", Tier::Payments, &["shippo"]),
    ("polymarket", Tier::Payments, &["polymarket"]),
    ("settlemint", Tier::Payments, &["settlemint"]),
    ("databento", Tier::Payments, &["databento"]),
    ("finnhub", Tier::Payments, &["finnhub"]),
    ("openai", Tier::AiProviders, &["openai"]),
    ("anthropic", Tier::AiProviders, &["anthropic"]),
    ("huggingface", Tier::AiProviders, &["huggingface"]),
    ("cohere", Tier::AiProviders, &["cohere"]),
    ("groq", Tier::AiProviders, &["groq"]),
    ("mistral", Tier::AiProviders, &["mistral"]),
    ("perplexity", Tier::AiProviders, &["perplexity"]),
    ("deepseek", Tier::AiProviders, &["deepseek"]),
    ("xai", Tier::AiProviders, &["xai", "grokxaiapikey"]),
    ("openrouter", Tier::AiProviders, &["openrouter"]),
    ("together ai", Tier::AiProviders, &["togetherai"]),
    ("replicate", Tier::AiProviders, &["replicate"]),
    ("cerebras", Tier::AiProviders, &["cerebras"]),
    ("nvidia", Tier::AiProviders, &["nvidia"]),
    ("fal", Tier::AiProviders, &["fal"]),
    ("stability", Tier::AiProviders, &["stability"]),
    ("elevenlabs", Tier::AiProviders, &["elevenlabs"]),
    ("deepgram", Tier::AiProviders, &["deepgram"]),
    ("assemblyai", Tier::AiProviders, &["assemblyai"]),
    ("voyage ai", Tier::AiProviders, &["voyageai"]),
    ("pinecone", Tier::AiProviders, &["pinecone"]),
    ("langchain", Tier::AiProviders, &["langchain"]),
    ("langfuse", Tier::AiProviders, &["langfuse"]),
    ("mem0", Tier::AiProviders, &["mem0"]),
    ("kimi", Tier::AiProviders, &["kimi"]),
    ("minimax", Tier::AiProviders, &["minimax"]),
    ("z.ai", Tier::AiProviders, &["zai"]),
    ("upstage", Tier::AiProviders, &["upstage"]),
    ("lighton", Tier::AiProviders, &["lighton"]),
    ("llama", Tier::AiProviders, &["llama"]),
    ("ollama", Tier::AiProviders, &["ollama"]),
    ("cartesia", Tier::AiProviders, &["cartesia"]),
    ("retell", Tier::AiProviders, &["retell"]),
    ("weights & biases", Tier::AiProviders, &["weights"]),
    ("labelbox", Tier::AiProviders, &["labelbox"]),
    ("private ai", Tier::AiProviders, &["privateai"]),
    ("cursor", Tier::AiProviders, &["cursor"]),
    ("devin", Tier::AiProviders, &["devin"]),
    ("greptile", Tier::AiProviders, &["greptile"]),
    ("mongodb", Tier::Databases, &["mongodb"]),
    ("planetscale", Tier::Databases, &["planetscale"]),
    ("neon", Tier::Databases, &["neon"]),
    ("snowflake", Tier::Databases, &["snowflake"]),
    ("databricks", Tier::Databases, &["databricks"]),
    ("clickhouse", Tier::Databases, &["clickhouse"]),
    ("cockroach labs", Tier::Databases, &["cockroachlabs"]),
    ("couchbase", Tier::Databases, &["couchbase"]),
    ("supabase", Tier::Databases, &["supabase"]),
    ("upstash", Tier::Databases, &["upstash"]),
    ("influxdb", Tier::Databases, &["influxdb"]),
    ("elastic", Tier::Databases, &["elastic"]),
    ("aiven", Tier::Databases, &["aiven"]),
    ("datastax", Tier::Databases, &["datastax"]),
    ("confluent", Tier::Databases, &["confluent"]),
    (
        "database connection string",
        Tier::Databases,
        &["connection"],
    ),
    ("airtable", Tier::Databases, &["airtable"]),
    ("algolia", Tier::Databases, &["algolia"]),
    ("tableau", Tier::Databases, &["tableau"]),
    ("looker", Tier::Databases, &["looker"]),
    ("segment", Tier::Databases, &["segment"]),
    ("amplitude", Tier::Databases, &["amplitude"]),
    ("posthog", Tier::Databases, &["posthog"]),
    (
        "slack",
        Tier::Messaging,
        &[
            "slack",
            "slackappconfigaccesstoken",
            "slackappconfigrefreshtoken",
            "slackappleveltoken",
        ],
    ),
    ("twilio", Tier::Messaging, &["twilio"]),
    ("sendgrid", Tier::Messaging, &["sendgrid"]),
    ("brevo", Tier::Messaging, &["sendinblue"]),
    ("mailgun", Tier::Messaging, &["mailgun"]),
    ("mailchimp", Tier::Messaging, &["mailchimp"]),
    ("mailersend", Tier::Messaging, &["mailersend"]),
    ("postmark", Tier::Messaging, &["postmark"]),
    ("resend", Tier::Messaging, &["resend"]),
    ("discord", Tier::Messaging, &["discord"]),
    ("telegram", Tier::Messaging, &["telegram"]),
    ("telnyx", Tier::Messaging, &["telnyx"]),
    ("plivo", Tier::Messaging, &["plivo"]),
    ("messagebird", Tier::Messaging, &["messagebird"]),
    ("sendbird", Tier::Messaging, &["sendbird"]),
    ("nylas", Tier::Messaging, &["nylas"]),
    ("onesignal", Tier::Messaging, &["onesignal"]),
    ("intercom", Tier::Messaging, &["intercom"]),
    ("mattermost", Tier::Messaging, &["mattermost"]),
    ("gitter", Tier::Messaging, &["gitter"]),
    ("lark", Tier::Messaging, &["lark"]),
    ("instantly", Tier::Messaging, &["instantly"]),
    ("klaviyo", Tier::Messaging, &["klaviyo"]),
    ("lob", Tier::Messaging, &["lob"]),
    ("datadog", Tier::Observability, &["datadog"]),
    ("grafana", Tier::Observability, &["grafana"]),
    ("sentry", Tier::Observability, &["sentry"]),
    ("new relic", Tier::Observability, &["new"]),
    ("dynatrace", Tier::Observability, &["dynatrace"]),
    ("honeycomb", Tier::Observability, &["honeycomb"]),
    ("pagerduty", Tier::Observability, &["pagerduty"]),
    ("opsgenie", Tier::Observability, &["opsgenie"]),
    ("rootly", Tier::Observability, &["rootly"]),
    ("sumo logic", Tier::Observability, &["sumologic"]),
    ("fullstory", Tier::Observability, &["fullstory"]),
    ("okta", Tier::IdentityAndSecrets, &["okta"]),
    ("auth0", Tier::IdentityAndSecrets, &["auth0"]),
    ("jumpcloud", Tier::IdentityAndSecrets, &["jumpcloud"]),
    ("clerk", Tier::IdentityAndSecrets, &["clerk"]),
    ("workos", Tier::IdentityAndSecrets, &["workos"]),
    ("authress", Tier::IdentityAndSecrets, &["authress"]),
    ("1password", Tier::IdentityAndSecrets, &["1password"]),
    (
        "bitwarden",
        Tier::IdentityAndSecrets,
        &["bitwardenoauth2access"],
    ),
    ("doppler", Tier::IdentityAndSecrets, &["doppler"]),
    ("age", Tier::IdentityAndSecrets, &["age"]),
    ("unkey", Tier::IdentityAndSecrets, &["unkey"]),
    ("private key", Tier::Generic, &["private"]),
    ("jwt", Tier::Generic, &["jwt"]),
    ("generic", Tier::Generic, &["generic"]),
    ("curl credentials", Tier::Generic, &["curl"]),
];

/// The rule id without its source prefix and numeric suffix:
/// `betterleaks.github-pat` -> `github-pat`, `veles.secrets/npmjsaccesstoken`
/// -> `npmjsaccesstoken`.
fn slug(rule_id: &str) -> &str {
    let s = rule_id.split_once('.').map_or(rule_id, |(_, rest)| rest);
    let s = s.strip_prefix("secrets/").unwrap_or(s);
    match s.rsplit_once('.') {
        Some((head, tail)) if tail.chars().all(|c| c.is_ascii_digit()) => head,
        _ => s,
    }
}

fn vendor_of(rule_id: &str) -> (String, Tier) {
    let (vendor, tier, _) = vendor_rank(rule_id);
    (vendor, tier)
}

/// The vendor, its tier, and its position in [`VENDORS`], which lists each
/// tier's vendors most common first. Unlisted vendors sort last.
fn vendor_rank(rule_id: &str) -> (String, Tier, usize) {
    let slug = slug(rule_id);
    let stem = slug.split('-').next().unwrap_or(slug);
    for (position, (vendor, tier, stems)) in VENDORS.iter().enumerate() {
        if stems.contains(&stem) {
            return ((*vendor).to_string(), *tier, position);
        }
    }
    (stem.to_string(), Tier::Other, usize::MAX)
}

/// dc-redact's shape names, read from the vendored table's source: the crate
/// does not export the table. Used only for the "claims this vendor" label.
const DC_REDACT_SOURCE: &str = include_str!("../../vendored/dc-redact/src/lib.rs");

/// FNV-1a of the vendored table's source as compiled into this run. `ENGINE`
/// is a constant and says which table was vendored; this says which one was
/// measured, so a run against a swapped-in older table labels itself.
fn dc_redact_fingerprint() -> String {
    format!("{:016x}", stable_seed(DC_REDACT_SOURCE))
}

fn dc_redact_shape_names() -> BTreeSet<String> {
    DC_REDACT_SOURCE
        .split("name: \"")
        .skip(1)
        .filter_map(|rest| rest.split_once('"').map(|(name, _)| name.to_string()))
        .collect()
}

fn squash(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// A dc-redact shape whose name mentions `vendor` (heuristic). A short
/// vendor name must be a whole word of the shape name, so `age` is not read
/// into "agent".
fn claimed_by<'a>(vendor: &str, shapes: &'a BTreeSet<String>) -> Option<&'a str> {
    let key = squash(match vendor {
        "google cloud" => "google",
        other => other,
    });
    if key.len() < 3 {
        return None;
    }
    shapes
        .iter()
        .find(|name| {
            if key.len() >= 5 {
                squash(name).contains(&key)
            } else {
                name.split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|word| word.eq_ignore_ascii_case(&key))
            }
        })
        .map(String::as_str)
}

// ---------------------------------------------------------------- scoring --

/// One rule's confirmed plant and what each detector made of it. For a plant
/// beside helper rules, a detector is credited only for a hit on the secret's
/// lines.
struct Scored {
    rule: usize,
    plant_length: Length,
    /// Hidden helper rules planted before the secret; empty for a plain plant.
    helpers: Vec<usize>,
    betterleaks: bool,
    dc_redact: Option<&'static str>,
    /// The detector flagged only helper lines of this plant:
    /// (Betterleaks, dc-redact).
    helper_line_only: (bool, bool),
    /// The first confirmed realistic-length candidate, when the plant is a
    /// minimum-length one: (Betterleaks, dc-redact) on it.
    realistic: Option<(bool, Option<&'static str>)>,
}

impl Scored {
    fn plant_label(&self) -> String {
        if self.helpers.is_empty() {
            self.plant_length.label().to_string()
        } else {
            format!("{} + helper", self.plant_length.label())
        }
    }
}

/// A plant under the reconstructed 2026-11-14 method: the first candidate
/// Kingfisher reports anything on, binned by that finding's confidence.
struct AnyScored {
    confidence: Confidence,
    betterleaks: bool,
    dc_redact: bool,
}

struct Measurement {
    kingfisher_version: String,
    betterleaks_version: String,
    rules: Vec<Rule>,
    hidden_rules: Vec<Rule>,
    /// Rules unconfirmed alone whose vendor stem has hidden helper rules ->
    /// which helpers were planted beside them, and which could not be sampled.
    paired: BTreeMap<usize, Pairing>,
    any: Vec<AnyScored>,
    betterleaks_rule_count: usize,
    validated: Vec<bool>,
    sampled: Vec<usize>,
    scored: Vec<Scored>,
    no_plant: BTreeMap<usize, NoPlant>,
    betterleaks_ids: BTreeSet<String>,
    shapes: BTreeSet<String>,
    plants: Vec<String>,
    corpus_variant: u64,
}

fn measure() -> Result<Measurement, String> {
    let kingfisher = oracle("kingfisher", "GITPULSE_KINGFISHER")?;
    let betterleaks = oracle("betterleaks", "GITPULSE_BETTERLEAKS")?;
    let tmp = tempfile::tempdir().map_err(|e| format!("temporary directory: {e}"))?;
    let scratch = tmp.path().join("scratch");
    let candidates_dir = tmp.path().join("candidates");
    let plants_dir = tmp.path().join("plants");
    for d in [&scratch, &candidates_dir, &plants_dir] {
        std::fs::create_dir(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }

    let kf_version = run(&kingfisher, &["--version"], &scratch, "kf-version")?;
    let bl_version = run(&betterleaks, &["version"], &scratch, "bl-version")?;
    if kf_version.status != 0 {
        return Err(failure(&kingfisher, "--version", &kf_version));
    }
    if bl_version.status != 0 {
        return Err(failure(&betterleaks, "version", &bl_version));
    }
    let (rules, hidden_rules) = kingfisher_rules(&kingfisher, &scratch)?;
    let betterleaks_ids = betterleaks_rule_ids(&betterleaks, &scratch)?;

    // Sample every rule; one file per candidate.
    let mut no_plant: BTreeMap<usize, NoPlant> = BTreeMap::new();
    let mut validated = vec![false; rules.len()];
    let mut sampled = vec![0usize; rules.len()];
    let mut candidates: Vec<Candidate> = Vec::new();
    let corpus_variant = corpus_variant()?;
    for (i, rule) in rules.iter().enumerate() {
        match sample_rule(i, rule, corpus_variant) {
            Ok((c, checked)) => {
                validated[i] = checked;
                sampled[i] = c.len();
                candidates.extend(c);
            }
            Err(why) => {
                no_plant.insert(i, why);
            }
        }
    }
    if candidates.is_empty() {
        return Err("the sampler produced no candidate for any rule".into());
    }
    for (n, c) in candidates.iter().enumerate() {
        write_file(&candidates_dir, &c.file_name(n), &c.text())?;
    }
    let kf = kingfisher_scan(&kingfisher, &candidates_dir, &scratch, "kf-scan")?;
    if kf.is_empty() {
        return Err(format!(
            "kingfisher reported nothing in {} candidates; refusing to score",
            candidates.len()
        ));
    }

    // The plant: the first candidate Kingfisher reports as its own rule.
    let mut chosen: BTreeMap<usize, (usize, Option<usize>)> = BTreeMap::new();
    let mut reported_elsewhere: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    // The reconstructed old method: first candidate with any finding, binned
    // by the confidence of the finding (its own rule's when reported).
    let mut any_plant: BTreeMap<usize, (usize, Confidence)> = BTreeMap::new();
    for (n, c) in candidates.iter().enumerate() {
        let found = kf.get(&c.file_name(n)).filter(|ids| !ids.is_empty());
        if let Some(ids) = found {
            let confidence = ids
                .get(&rules[c.rule].id)
                .map(|f| f.confidence)
                .or_else(|| ids.values().map(|f| f.confidence).min())
                .expect("non-empty findings");
            any_plant.entry(c.rule).or_insert((n, confidence));
        }
        let own = found.is_some_and(|ids| ids.contains_key(&rules[c.rule].id));
        if !own {
            if let Some(ids) = found {
                reported_elsewhere
                    .entry(c.rule)
                    .or_default()
                    .extend(ids.keys().cloned());
            }
            continue;
        }
        choose(&mut chosen, &candidates, n);
    }
    for (i, n) in sampled.iter().enumerate() {
        if *n > 0 && !chosen.contains_key(&i) {
            no_plant.insert(
                i,
                NoPlant::NotConfirmed {
                    kingfisher_reported: reported_elsewhere.remove(&i).unwrap_or_default(),
                },
            );
        }
    }

    // Rules Kingfisher confirmed on no candidate alone: plant every candidate
    // again after one sample of each hidden helper rule with the same vendor
    // stem, scanned on its own. Appended, so the plain candidates' indices,
    // names and plants do not move.
    let first_scan = candidates.len();
    let mut helper_samples: BTreeMap<usize, Option<String>> = BTreeMap::new();
    let mut paired: BTreeMap<usize, Pairing> = BTreeMap::new();
    let unconfirmed: Vec<usize> = no_plant
        .iter()
        .filter(|(_, why)| matches!(why, NoPlant::NotConfirmed { .. }))
        .map(|(i, _)| *i)
        .collect();
    for i in unconfirmed {
        let helpers = helper_indices(&rules[i].id, &hidden_rules);
        if helpers.is_empty() {
            continue;
        }
        let mut prefix = String::new();
        let mut pairing = Pairing::default();
        for h in helpers {
            let sample = helper_samples.entry(h).or_insert_with(|| {
                sample_rule(h, &hidden_rules[h], corpus_variant)
                    .ok()
                    .and_then(|(c, _)| c.into_iter().next())
                    .map(|c| c.secret)
            });
            match sample {
                Some(s) => {
                    prefix.push_str(s);
                    prefix.push('\n');
                    pairing.planted.push(h);
                }
                None => pairing.unsampled.push(h),
            }
        }
        if !pairing.planted.is_empty() {
            let beside: Vec<Candidate> = candidates[..first_scan]
                .iter()
                .filter(|c| c.rule == i)
                .map(|c| Candidate {
                    prefix: prefix.clone(),
                    helpers: pairing.planted.clone(),
                    ..c.clone()
                })
                .collect();
            candidates.extend(beside);
        }
        paired.insert(i, pairing);
    }
    if candidates.len() > first_scan {
        let paired_dir = tmp.path().join("paired");
        std::fs::create_dir(&paired_dir).map_err(|e| format!("{}: {e}", paired_dir.display()))?;
        for (n, c) in candidates.iter().enumerate().skip(first_scan) {
            write_file(&paired_dir, &c.file_name(n), &c.text())?;
        }
        let kf_paired = kingfisher_scan(&kingfisher, &paired_dir, &scratch, "kf-scan-paired")?;
        for (n, c) in candidates.iter().enumerate().skip(first_scan) {
            let own = kf_paired
                .get(&c.file_name(n))
                .and_then(|ids| ids.get(&rules[c.rule].id))
                .is_some_and(|f| f.lines.iter().any(|l| *l >= c.secret_line()));
            if own {
                choose(&mut chosen, &candidates, n);
                no_plant.remove(&c.rule);
            }
        }
    }

    // Betterleaks and dc-redact on the confirmed plants (and their
    // realistic-length counterparts).
    let mut to_score: Vec<usize> = Vec::new();
    for (plant, realistic) in chosen.values() {
        to_score.push(*plant);
        to_score.extend(realistic);
    }
    to_score.extend(any_plant.values().map(|(n, _)| *n));
    to_score.sort_unstable();
    to_score.dedup();
    for n in &to_score {
        let c = &candidates[*n];
        write_file(&plants_dir, &c.file_name(*n), &c.text())?;
    }
    let bl = betterleaks_scan(&betterleaks, &plants_dir, &scratch)?;
    if bl.is_empty() {
        return Err(format!(
            "betterleaks reported nothing in {} plants; refusing to score",
            to_score.len()
        ));
    }
    // (Betterleaks, dc-redact, hits on helper lines only) for one plant.
    let judge = |n: usize| -> Result<Judged, String> {
        let c = &candidates[n];
        let name = c.file_name(n);
        let bl_lines = bl.get(&name);
        if c.helpers.is_empty() {
            return Ok((
                bl_lines.is_some(),
                dc_redact(&name, &c.secret),
                (false, false),
            ));
        }
        let b = place(bl_lines.into_iter().flatten().copied(), c.secret_line());
        let (dc, dc_helper_only) = dc_redact_placed(&name, c);
        // dc-redact reads one line at a time, so the helpers beside the
        // secret must not change its verdict on the secret.
        if dc.is_some() != dc_redact(&name, &c.secret).is_some() {
            return Err(format!(
                "dc-redact judged {name}'s secret differently beside its helpers than alone"
            ));
        }
        Ok((b.secret, dc, (b.helper_only, dc_helper_only)))
    };
    let mut scored: Vec<Scored> = Vec::new();
    for (rule, (plant, realistic)) in &chosen {
        let (betterleaks, dc_redact, helper_line_only) = judge(*plant)?;
        let realistic = match realistic {
            Some(n) => {
                let (b, d, _) = judge(*n)?;
                Some((b, d))
            }
            None => None,
        };
        scored.push(Scored {
            rule: *rule,
            plant_length: candidates[*plant].length,
            helpers: candidates[*plant].helpers.clone(),
            betterleaks,
            dc_redact,
            helper_line_only,
            realistic,
        });
    }
    let mut any: Vec<AnyScored> = Vec::new();
    for (n, confidence) in any_plant.values() {
        let (betterleaks, dc, _) = judge(*n)?;
        any.push(AnyScored {
            confidence: *confidence,
            betterleaks,
            dc_redact: dc.is_some(),
        });
    }

    Ok(Measurement {
        kingfisher_version: first_line(&kf_version.stdout),
        betterleaks_version: first_line(&bl_version.stdout),
        rules,
        hidden_rules,
        paired,
        any,
        betterleaks_rule_count: betterleaks_ids.len(),
        validated,
        sampled,
        scored,
        no_plant,
        betterleaks_ids,
        shapes: dc_redact_shape_names(),
        plants: candidates.iter().map(Candidate::text).collect(),
        corpus_variant,
    })
}

/// The hidden helper rules sharing an unconfirmed rule's vendor stem.
#[derive(Debug, Clone, Default)]
struct Pairing {
    /// Sampled and planted on the lines before the rule's candidates.
    planted: Vec<usize>,
    /// The sampler produced no candidate matching the helper's regex.
    unsampled: Vec<usize>,
}

/// (Betterleaks on the secret, dc-redact's shape on the secret, and whether
/// each flagged only helper lines).
type Judged = (bool, Option<&'static str>, (bool, bool));

/// Record candidate `n` as its rule's plant when it is the first confirmed,
/// or as the plant's realistic-length counterpart.
fn choose(
    chosen: &mut BTreeMap<usize, (usize, Option<usize>)>,
    candidates: &[Candidate],
    n: usize,
) {
    let c = &candidates[n];
    let entry = chosen.entry(c.rule).or_insert((n, None));
    if c.length == Length::Realistic
        && candidates[entry.0].length == Length::Min
        && entry.1.is_none()
    {
        entry.1 = Some(n);
    }
}

// ----------------------------------------------------------------- report --

fn dc_reason(m: &Measurement, s: &Scored) -> Option<String> {
    if s.dc_redact.is_some() {
        return None;
    }
    let id = &m.rules[s.rule].id;
    let (vendor, _) = vendor_of(id);
    let helper_note = if s.helper_line_only.1 {
        " (it flags only a planted helper line, which is not credited)"
    } else {
        ""
    };
    if let Some((_, Some(shape))) = s.realistic {
        return Some(format!(
            "dc-redact: the minimum-length plant is under its floor; the realistic-length plant is found as `{shape}` (artefact)"
        ));
    }
    Some(match claimed_by(&vendor, &m.shapes) {
        Some(shape) => format!(
            "dc-redact: names {vendor} (`{shape}`) but has no shape for this format (heuristic){helper_note}"
        ),
        None => format!("dc-redact: no shape for {vendor}{helper_note}"),
    })
}

fn betterleaks_reason(m: &Measurement, s: &Scored) -> Option<String> {
    if s.betterleaks {
        return None;
    }
    let id = &m.rules[s.rule].id;
    let Some(bl_id) = id.strip_prefix("betterleaks.") else {
        let source = id.split(['.', '/']).next().unwrap_or(id);
        return Some(format!(
            "Betterleaks: a Kingfisher `{source}` rule with no Betterleaks counterpart"
        ));
    };
    if !m.betterleaks_ids.contains(bl_id) {
        return Some(format!("Betterleaks: no rule `{bl_id}` in its config"));
    }
    if s.helper_line_only.0 {
        return Some(format!(
            "Betterleaks: has `{bl_id}` but reports only a planted helper line, which is not credited"
        ));
    }
    if let Some((true, _)) = s.realistic {
        return Some(
            "Betterleaks: misses the minimum-length plant, finds the realistic-length one".into(),
        );
    }
    Some(format!(
        "Betterleaks: has `{bl_id}` but its keyword, entropy or allowlist filters drop this plant"
    ))
}

/// Kingfisher's hidden helper rules for the same vendor stem as `rule_id`.
/// Choosing helpers by stem is a heuristic; Kingfisher confirming the rule
/// beside them is the evidence.
fn helpers_for<'a>(rule_id: &str, hidden: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let stem = |id: &str| -> String {
        let s = slug(id);
        s.split('-').next().unwrap_or(s).to_string()
    };
    let own = stem(rule_id);
    hidden.into_iter().filter(|h| stem(h) == own).collect()
}

/// [`helpers_for`] as indices into `hidden`.
fn helper_indices(rule_id: &str, hidden: &[Rule]) -> Vec<usize> {
    let ids = helpers_for(rule_id, hidden.iter().map(|h| h.id.as_str()));
    (0..hidden.len())
        .filter(|i| ids.contains(&hidden[*i].id.as_str()))
        .collect()
}

fn helper_names(m: &Measurement, helpers: &[usize]) -> String {
    helpers
        .iter()
        .map(|h| format!("`{}`", m.hidden_rules[*h].id))
        .collect::<Vec<_>>()
        .join(", ")
}

fn no_plant_reason(m: &Measurement, rule: usize, why: &NoPlant) -> String {
    match why {
        NoPlant::Unsupported(e) => format!("sampler refuses the pattern: {e}"),
        NoPlant::NoMatchingSample => {
            "no sample matched the rule's own regex (checked with the regex crate)".into()
        }
        NoPlant::NotConfirmed {
            kingfisher_reported,
        } => {
            let alone = if kingfisher_reported.is_empty() {
                "Kingfisher reported nothing on any candidate".to_string()
            } else {
                format!(
                    "Kingfisher reported other rules on its candidates: {}",
                    kingfisher_reported
                        .iter()
                        .map(|r| format!("`{r}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            let unsampled = |p: &Pairing| {
                if p.unsampled.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; the sampler could not produce helper(s) {}, which were not planted",
                        helper_names(m, &p.unsampled)
                    )
                }
            };
            match m.paired.get(&rule) {
                Some(p) if p.planted.is_empty() => format!(
                    "{alone}{}, so no helper was planted and the cause is not established",
                    unsampled(p)
                ),
                Some(p) => format!(
                    "{alone}; not confirmed beside hidden helper(s) {} either{}, so the cause is not established",
                    helper_names(m, &p.planted),
                    unsampled(p)
                ),
                None if kingfisher_reported.is_empty() => format!(
                    "{alone}; no hidden helper rule shares its vendor, so the cause is not established (a filter the regex does not show)"
                ),
                None => alone,
            }
        }
    }
}

/// Plants, and each detector's hits on them, per rule confidence, over the
/// scored rules `keep` selects.
fn recall_table(
    r: &mut String,
    m: &Measurement,
    confidences: &BTreeSet<Confidence>,
    keep: impl Fn(&Scored) -> bool,
) {
    let _ = writeln!(
        r,
        "| confidence | plants | Kingfisher | Betterleaks | dc-redact |"
    );
    let _ = writeln!(r, "| --- | ---: | ---: | ---: | ---: |");
    for conf in confidences {
        let rows: Vec<&Scored> = m
            .scored
            .iter()
            .filter(|s| m.rules[s.rule].confidence == *conf && keep(s))
            .collect();
        let n = rows.len();
        let bl = rows.iter().filter(|s| s.betterleaks).count();
        let dc = rows.iter().filter(|s| s.dc_redact.is_some()).count();
        let _ = writeln!(
            r,
            "| {} | {n} | {n}/{n} | {bl}/{n} | {dc}/{n} |",
            conf.label()
        );
    }
}

fn yes(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "**no**"
    }
}

fn render(m: &Measurement) -> String {
    let mut r = String::new();
    let confidences: BTreeSet<Confidence> = m.rules.iter().map(|r| r.confidence).collect();
    let total_candidates: usize = m.sampled.iter().sum();
    let _ = writeln!(r, "# Commit secret scan recall\n");
    let _ = writeln!(
        r,
        "- Detector: `{ENGINE}` through `leak_scan::Tally`; measured table source FNV-1a `{}`, {} shape names",
        dc_redact_fingerprint(),
        m.shapes.len()
    );
    let _ = writeln!(
        r,
        "- Kingfisher: `{}`, {} visible rules ({} helper rules not reported on their own)",
        m.kingfisher_version,
        m.rules.len(),
        m.hidden_rules.len()
    );
    let _ = writeln!(
        r,
        "- Betterleaks: `{}`, {} rules in its default config",
        m.betterleaks_version, m.betterleaks_rule_count
    );
    let _ = writeln!(
        r,
        "- Plants: up to {MIN_CANDIDATES} minimum-length and {REALISTIC_CANDIDATES} realistic-length candidates per rule, \
         seeded by the rule id (salt {}); {total_candidates} candidates. A rule's plant is the first candidate Kingfisher reports \
         as that rule, minimum-length first, so **Kingfisher is 100% by construction**.",
        m.corpus_variant
    );
    let _ = writeln!(
        r,
        "- Helper rules: a rule Kingfisher confirms on no candidate alone is planted again with one sample of each hidden \
         helper rule of the same vendor stem on the lines before it, and scanned again ({} rules have such a helper; {} \
         were planted beside at least one). Those plants are reported apart from the comparable table; on them a \
         detector is credited only for a hit on the secret's lines.",
        m.paired.len(),
        m.paired.values().filter(|p| !p.planted.is_empty()).count()
    );
    let _ = writeln!(r, "- No live validation: Kingfisher `--no-validate`, Betterleaks without `--validation`, both `--redact`.\n");

    let _ = writeln!(r, "## Funnel\n");
    let _ = writeln!(
        r,
        "| confidence | visible rules | sampler refused | no sample matched | regex crate rejects (unchecked) | not confirmed by Kingfisher | plants | confirmed beside a helper | still unplanted |"
    );
    let _ = writeln!(
        r,
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
    );
    for conf in &confidences {
        let in_conf = |i: &usize| m.rules[*i].confidence == *conf;
        let rules = (0..m.rules.len()).filter(in_conf).count();
        let count = |f: &dyn Fn(&NoPlant) -> bool| {
            m.no_plant
                .iter()
                .filter(|(i, w)| in_conf(i) && f(w))
                .count()
        };
        let refused = count(&|w| matches!(w, NoPlant::Unsupported(_)));
        let unmatched = count(&|w| matches!(w, NoPlant::NoMatchingSample));
        let still = count(&|w| matches!(w, NoPlant::NotConfirmed { .. }));
        let unchecked = (0..m.rules.len())
            .filter(|i| in_conf(i) && m.sampled[*i] > 0 && !m.validated[*i])
            .count();
        let plants = m
            .scored
            .iter()
            .filter(|s| in_conf(&s.rule) && s.helpers.is_empty())
            .count();
        let beside = m
            .scored
            .iter()
            .filter(|s| in_conf(&s.rule) && !s.helpers.is_empty())
            .count();
        // Unconfirmed alone: still unplanted, or confirmed beside a helper.
        let unconfirmed = still + beside;
        let _ = writeln!(
            r,
            "| {} | {rules} | {refused} | {unmatched} | {unchecked} | {unconfirmed} | {plants} | {beside} | {} |",
            conf.label(),
            refused + unmatched + still
        );
    }

    let _ = writeln!(r, "\n## Recall on the confirmed plants\n");
    let _ = writeln!(
        r,
        "The comparable table: each rule's first confirmed candidate, at minimum length unless only a realistic-length one was confirmed.\n"
    );
    recall_table(&mut r, m, &confidences, |s| s.helpers.is_empty());

    let _ = writeln!(r, "\n## At realistic length\n");
    let _ = writeln!(
        r,
        "Rules whose plant is minimum-length and that also have a confirmed realistic-length candidate: the same detectors on that candidate.\n"
    );
    let _ = writeln!(
        r,
        "| confidence | rules | Betterleaks min / realistic | dc-redact min / realistic |"
    );
    let _ = writeln!(r, "| --- | ---: | ---: | ---: |");
    for conf in &confidences {
        let rows: Vec<(&Scored, (bool, Option<&'static str>))> = m
            .scored
            .iter()
            .filter(|s| m.rules[s.rule].confidence == *conf && s.helpers.is_empty())
            .filter_map(|s| s.realistic.map(|real| (s, real)))
            .collect();
        let n = rows.len();
        let bl_min = rows.iter().filter(|(s, _)| s.betterleaks).count();
        let bl_real = rows.iter().filter(|(_, (b, _))| *b).count();
        let dc_min = rows.iter().filter(|(s, _)| s.dc_redact.is_some()).count();
        let dc_real = rows.iter().filter(|(_, (_, d))| d.is_some()).count();
        let _ = writeln!(
            r,
            "| {} | {n} | {bl_min} / {bl_real} | {dc_min} / {dc_real} |",
            conf.label()
        );
    }

    let _ = writeln!(r, "\n## Beside their helper rules\n");
    let _ = writeln!(
        r,
        "Rules Kingfisher confirmed on no candidate alone, confirmed once the samples of their vendor's hidden helper \
         rules were planted on the lines before them. Each is verified as needing a helper on the same input; which \
         helper was chosen by vendor stem, a heuristic. Not part of the comparable table above. A detector that flags \
         only a helper line is not credited; the last two columns count those.\n"
    );
    let _ = writeln!(
        r,
        "| confidence | plants | Kingfisher | Betterleaks | dc-redact | Betterleaks on a helper line only | dc-redact on a helper line only |"
    );
    let _ = writeln!(r, "| --- | ---: | ---: | ---: | ---: | ---: | ---: |");
    for conf in &confidences {
        let rows: Vec<&Scored> = m
            .scored
            .iter()
            .filter(|s| m.rules[s.rule].confidence == *conf && !s.helpers.is_empty())
            .collect();
        let n = rows.len();
        let bl = rows.iter().filter(|s| s.betterleaks).count();
        let dc = rows.iter().filter(|s| s.dc_redact.is_some()).count();
        let bl_helper = rows.iter().filter(|s| s.helper_line_only.0).count();
        let dc_helper = rows.iter().filter(|s| s.helper_line_only.1).count();
        let _ = writeln!(
            r,
            "| {} | {n} | {n}/{n} | {bl}/{n} | {dc}/{n} | {bl_helper} | {dc_helper} |",
            conf.label()
        );
    }

    let _ = writeln!(
        r,
        "\n| rule | confidence | helper rule(s) planted | Betterleaks | dc-redact |"
    );
    let _ = writeln!(r, "| --- | --- | --- | --- | --- |");
    for s in m.scored.iter().filter(|s| !s.helpers.is_empty()) {
        let rule = &m.rules[s.rule];
        let dc = s
            .dc_redact
            .map(|shape| format!("yes (`{shape}`)"))
            .unwrap_or_else(|| yes(false).into());
        let _ = writeln!(
            r,
            "| `{}` | {} | {} | {} | {dc} |",
            rule.id,
            rule.confidence.label(),
            helper_names(m, &s.helpers),
            yes(s.betterleaks),
        );
    }

    let _ = writeln!(r, "\n## Every measured rule\n");
    let _ = writeln!(
        r,
        "The two tables above together: every rule with a confirmed plant, alone or beside its helpers.\n"
    );
    recall_table(&mut r, m, &confidences, |_| true);

    // Vendors dc-redact misses, ranked.
    // (tier, position in VENDORS, vendor) -> (rules missed, rules with no
    // plant). A rule with no plant was never measured, so its vendor must not
    // read as covered.
    type MissKey = (Tier, usize, String);
    type MissRow<'a> = (Vec<&'a str>, Vec<&'a str>);
    let mut misses: BTreeMap<MissKey, MissRow> = BTreeMap::new();
    let key = |id: &str| {
        let (vendor, tier, position) = vendor_rank(id);
        (tier, position, vendor)
    };
    for s in &m.scored {
        let real_hit = matches!(s.realistic, Some((_, Some(_))));
        if s.dc_redact.is_none() && !real_hit {
            let id = m.rules[s.rule].id.as_str();
            misses.entry(key(id)).or_default().0.push(id);
        }
    }
    for i in m.no_plant.keys() {
        let id = m.rules[*i].id.as_str();
        misses.entry(key(id)).or_default().1.push(id);
    }
    let mut ranked: Vec<(MissKey, MissRow)> = misses.into_iter().collect();
    // Tier, then the curated commonness order; unlisted vendors by rules
    // missed, then name.
    ranked.sort_by(|((ta, pa, va), a), ((tb, pb, vb), b)| {
        ta.cmp(tb)
            .then(pa.cmp(pb))
            .then((b.0.len() + b.1.len()).cmp(&(a.0.len() + a.1.len())))
            .then(va.cmp(vb))
    });
    let _ = writeln!(r, "\n## Vendors dc-redact misses, ranked\n");
    let _ = writeln!(
        r,
        "Misses at both lengths (length artefacts excluded), and rules with no confirmed plant, which were **not measured** \
         (see the last section for why). Ranked by tier — cloud, CI and packages, payments, AI providers, databases first — \
         then by the curated commonness order of `VENDORS` in `recall_tests.rs`. \
         \"dc-redact names it\" is a heuristic: a shape name in the vendored table mentions the vendor.\n"
    );
    let _ = writeln!(
        r,
        "| rank | tier | vendor | rules missed | not measured | dc-redact names it | rules missed | rules not measured |"
    );
    let _ = writeln!(r, "| ---: | --- | --- | ---: | ---: | --- | --- | --- |");
    let list = |ids: &[&str]| {
        if ids.is_empty() {
            "—".to_string()
        } else {
            ids.iter()
                .map(|i| format!("`{i}`"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    for (rank, ((tier, _, vendor), (missed, unmeasured))) in ranked.iter().enumerate() {
        let named = claimed_by(vendor, &m.shapes)
            .map(|s| format!("`{s}`"))
            .unwrap_or_else(|| "no".into());
        let _ = writeln!(
            r,
            "| {} | {} | {vendor} | {} | {} | {named} | {} | {} |",
            rank + 1,
            tier.label(),
            missed.len(),
            unmeasured.len(),
            list(missed),
            list(unmeasured),
        );
    }

    let _ = writeln!(
        r,
        "\n## The 2026-11-14 method, reconstructed (hypothesis)\n"
    );
    let _ = writeln!(
        r,
        "That corpus is lost. This is one guess at how it counted: a rule's plant is its first candidate Kingfisher reports \
         *anything* on (a sibling rule included), binned by the confidence of that finding rather than the rule listing. \
         It differs from the tables above only for rules whose bytes a sibling rule claims.\n"
    );
    let _ = writeln!(
        r,
        "| finding confidence | plants | Kingfisher | Betterleaks | dc-redact |"
    );
    let _ = writeln!(r, "| --- | ---: | ---: | ---: | ---: |");
    let any_confidences: BTreeSet<Confidence> = m.any.iter().map(|a| a.confidence).collect();
    for conf in &any_confidences {
        let rows: Vec<&AnyScored> = m.any.iter().filter(|a| a.confidence == *conf).collect();
        let n = rows.len();
        let bl = rows.iter().filter(|a| a.betterleaks).count();
        let dc = rows.iter().filter(|a| a.dc_redact).count();
        let _ = writeln!(
            r,
            "| {} | {n} | {n}/{n} | {bl}/{n} | {dc}/{n} |",
            conf.label()
        );
    }

    let _ = writeln!(r, "\n## Per-rule results\n");
    let _ = writeln!(
        r,
        "| rule | confidence | plant | Kingfisher | Betterleaks | dc-redact | realistic: Betterleaks / dc-redact |"
    );
    let _ = writeln!(r, "| --- | --- | --- | --- | --- | --- | --- |");
    for s in &m.scored {
        let rule = &m.rules[s.rule];
        let dc = s
            .dc_redact
            .map(|shape| format!("yes (`{shape}`)"))
            .unwrap_or_else(|| yes(false).into());
        let realistic = s
            .realistic
            .map(|(b, d)| format!("{} / {}", yes(b), yes(d.is_some())))
            .unwrap_or_else(|| "—".into());
        let _ = writeln!(
            r,
            "| `{}` | {} | {} | yes | {} | {dc} | {realistic} |",
            rule.id,
            rule.confidence.label(),
            s.plant_label(),
            yes(s.betterleaks),
        );
    }

    let _ = writeln!(r, "\n## Every disagreement\n");
    let _ = writeln!(
        r,
        "| rule | confidence | plant | Kingfisher | Betterleaks | dc-redact | reason |"
    );
    let _ = writeln!(r, "| --- | --- | --- | --- | --- | --- | --- |");
    for s in &m.scored {
        let reasons: Vec<String> = [dc_reason(m, s), betterleaks_reason(m, s)]
            .into_iter()
            .flatten()
            .collect();
        if reasons.is_empty() {
            continue;
        }
        let rule = &m.rules[s.rule];
        let dc = s
            .dc_redact
            .map(|shape| format!("yes (`{shape}`)"))
            .unwrap_or_else(|| yes(false).into());
        let _ = writeln!(
            r,
            "| `{}` | {} | {} | yes | {} | {dc} | {} |",
            rule.id,
            rule.confidence.label(),
            s.plant_label(),
            yes(s.betterleaks),
            reasons.join("; ")
        );
    }

    let _ = writeln!(r, "\n## Rules without a confirmed plant\n");
    let _ = writeln!(r, "| rule | confidence | reason |");
    let _ = writeln!(r, "| --- | --- | --- |");
    for (i, why) in &m.no_plant {
        let rule = &m.rules[*i];
        let _ = writeln!(
            r,
            "| `{}` | {} | {} |",
            rule.id,
            rule.confidence.label(),
            no_plant_reason(m, *i, why)
        );
    }
    r
}

/// Fail when any credential-looking run of a plant (16 or more characters of
/// a token alphabet) appears in `report`.
fn assert_no_plant_in(report: &str, plants: &[String]) {
    for (n, plant) in plants.iter().enumerate() {
        for run in plant.split(|c: char| !is_token_char(c)) {
            assert!(
                run.len() < 16 || !report.contains(run),
                "the report carries part of plant {n}; refusing to print or write it"
            );
        }
    }
}

#[test]
#[ignore = "measurement: runs Kingfisher and Betterleaks; see docs/PERFORMANCE.md"]
fn measure_recall() {
    let m = measure().unwrap_or_else(|e| panic!("recall measurement failed: {e}"));
    let report = render(&m);
    assert_no_plant_in(&report, &m.plants);
    print!("{report}");
    if let Some(out) = std::env::var_os("GITPULSE_LEAK_RECALL_OUT") {
        std::fs::write(&out, &report)
            .unwrap_or_else(|e| panic!("writing {}: {e}", PathBuf::from(&out).display()));
    }
}

#[test]
fn a_missing_oracle_is_an_error_never_a_zero() {
    let none = resolve_oracle("kingfisher", "GITPULSE_KINGFISHER", None, |_| None)
        .expect_err("not installed");
    assert!(none.contains("never scored as zero"), "{none}");

    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("no-such-binary");
    let bad = resolve_oracle(
        "betterleaks",
        "GITPULSE_BETTERLEAKS",
        Some(gone.clone().into_os_string()),
        |_| Some("/bin/sh".into()),
    )
    .expect_err("an override naming nothing must not fall back to PATH");
    assert!(bad.contains("GITPULSE_BETTERLEAKS"), "{bad}");

    let found = resolve_oracle("x", "X", None, |_| Some("/bin/sh".into())).unwrap();
    assert_eq!(found, PathBuf::from("/bin/sh"));
}

#[test]
fn rule_ids_map_to_vendors_and_tiers() {
    assert_eq!(slug("betterleaks.github-pat"), "github-pat");
    assert_eq!(slug("betterleaks.abuseipdb-api-key.1"), "abuseipdb-api-key");
    assert_eq!(slug("veles.secrets/npmjsaccesstoken"), "npmjsaccesstoken");
    assert_eq!(
        slug("kingfisher.connection-string.password.unquoted"),
        "connection-string.password.unquoted"
    );
    assert_eq!(
        vendor_of("betterleaks.aws-access-token"),
        ("aws".into(), Tier::Cloud)
    );
    assert_eq!(
        vendor_of("veles.secrets/npmjsaccesstoken"),
        ("npm".into(), Tier::CiAndPackages)
    );
    assert_eq!(
        vendor_of("kingfisher.connection-string.password.unquoted"),
        ("database connection string".into(), Tier::Databases)
    );
    assert_eq!(
        vendor_of("betterleaks.zendesk-secret-key"),
        ("zendesk".into(), Tier::Other)
    );
    let mut seen = BTreeSet::new();
    for (vendor, _, stems) in VENDORS {
        assert!(!stems.is_empty(), "{vendor}");
        for stem in *stems {
            assert!(
                seen.insert(*stem),
                "stem `{stem}` listed twice; the first wins"
            );
        }
    }
}

#[test]
fn shape_names_come_from_the_vendored_table() {
    let shapes = dc_redact_shape_names();
    assert!(shapes.len() >= 40, "{} shapes", shapes.len());
    assert!(claimed_by("github", &shapes).is_some());
    assert!(claimed_by("aws", &shapes).is_some());
    assert!(claimed_by("zendesk", &shapes).is_none());
    // A short vendor name is a whole word, not a substring of "agent".
    let agent: BTreeSet<String> = ["gitlab agent for kubernetes token".to_string()].into();
    assert!(claimed_by("age", &agent).is_none());
    assert!(claimed_by("gitlab", &agent).is_some());
}

#[test]
fn a_report_that_carries_a_plant_is_refused() {
    // Low-entropy on purpose: no scanner should read this file as a leak.
    let token = format!("pre_{}", "Ab1".repeat(7));
    let plants = vec![format!("key = \"{token}\"")];
    assert_no_plant_in("| `betterleaks.pre-token` | high |", &plants);
    let report = format!("value {token} in a row");
    let leaked = std::panic::catch_unwind(|| assert_no_plant_in(&report, &plants));
    assert!(leaked.is_err());
}

#[test]
fn oracle_stderr_is_redacted_before_it_reaches_a_message() {
    let run = "Ab1".repeat(7);
    let text = format!("error: bad input near {run}: stop");
    let out = redact_runs(&text);
    assert!(!out.contains(&run), "{out}");
    assert_eq!(out, "error: bad input near [run redacted]: stop");
}

#[test]
fn helper_rules_are_matched_by_vendor_stem() {
    let hidden = [
        "betterleaks.aikido-client-id",
        "betterleaks.azure-tenant-id",
    ];
    assert_eq!(
        helpers_for("betterleaks.aikido-client-secret", hidden),
        vec!["betterleaks.aikido-client-id"]
    );
    assert!(helpers_for("betterleaks.slack-webhook-url", hidden).is_empty());

    let rules: Vec<Rule> = hidden
        .iter()
        .map(|id| Rule {
            id: (*id).to_string(),
            confidence: Confidence::High,
            pattern: String::new(),
        })
        .collect();
    assert_eq!(
        helper_indices("betterleaks.azure-client-secret", &rules),
        vec![1]
    );
}

#[test]
fn the_secret_line_counts_newlines_not_helpers() {
    assert_eq!(secret_line_after(""), 1);
    assert_eq!(secret_line_after("first helper\nsecond helper\n"), 3);
    // One helper whose sample spans two lines.
    assert_eq!(secret_line_after("vendor id\n= value\n"), 3);

    let mut c = Candidate::plain(0, Length::Min, "the secret".into());
    assert_eq!(c.secret_line(), 1);
    assert_eq!(c.text(), "the secret");
    c.prefix = "helper\n".into();
    c.helpers = vec![0];
    assert_eq!(c.secret_line(), 2);
    assert_eq!(c.text(), "helper\nthe secret");
    assert!(c.file_name(7).starts_with("p00007-"));
}

#[test]
fn hits_are_placed_on_the_secret_or_only_on_a_helper() {
    let none = place([], 3);
    assert_eq!(none, Placed::default());
    let helper = place([1, 2], 3);
    assert!(!helper.secret && helper.helper_only);
    let both = place([1, 3], 3);
    assert!(both.secret && !both.helper_only);
    // A secret that spans lines: a hit past its first line is still its own.
    let later = place([4], 3);
    assert!(later.secret && !later.helper_only);
}

#[test]
fn a_helper_line_hit_is_not_credited_to_the_secret() {
    // A PEM header dc-redact flags, built at run time, as the helper line.
    let header = format!("{} RSA PRIVATE KEY-----", "-----BEGIN");
    let mut c = Candidate::plain(0, Length::Min, "no credential on this line".into());
    c.prefix = format!("{header}\n");
    c.helpers = vec![0];
    let (shape, helper_only) = dc_redact_placed("p.txt", &c);
    assert_eq!(shape, None);
    assert!(helper_only);

    // The same header as the secret is credited.
    let c = Candidate {
        prefix: "an ordinary helper line\n".into(),
        secret: header,
        helpers: vec![0],
        ..Candidate::plain(0, Length::Min, String::new())
    };
    let (shape, helper_only) = dc_redact_placed("p.txt", &c);
    assert!(shape.is_some());
    assert!(!helper_only);
}

#[test]
fn every_kingfisher_style_seed_is_stable() {
    assert_eq!(
        stable_seed("betterleaks.github-pat"),
        stable_seed("betterleaks.github-pat")
    );
    assert_ne!(
        stable_seed("betterleaks.github-pat"),
        stable_seed("betterleaks.github-oauth")
    );
}
