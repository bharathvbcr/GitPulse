//! Recall and precision corpus for the engine.
//!
//! Recall: one planted credential per shape the table claims, in the vendor's
//! real format and length, so the same bytes can be handed to Kingfisher and
//! Betterleaks for comparison (`dump_corpus_for_oracles`, below). Every value
//! is generated at run time from a fixed seed: this file holds no token.
//!
//! Precision: lines a repository is full of that are not credentials —
//! lockfile hashes, object ids, UUIDs, minified code, data URLs, references to
//! a secret rather than the secret — and must never block a commit.

use super::Tally;

/// Deterministic generator, so a failure names the same token every run.
struct Seeded(u64);

impl Seeded {
    fn next(&mut self) -> u64 {
        // SplitMix64.
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn take(&mut self, alphabet: &str, n: usize) -> String {
        let chars: Vec<char> = alphabet.chars().collect();
        (0..n)
            .map(|_| chars[(self.next() % chars.len() as u64) as usize])
            .collect()
    }
}

const ALNUM: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const UPPER_ALNUM: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
const HEX: &str = "0123456789abcdef";
const URLSAFE: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const LOWER: &str = "abcdefghijklmnopqrstuvwxyz";

/// (shape name dc-redact must report, the line it sits on).
pub(super) fn planted() -> Vec<(&'static str, String)> {
    let mut r = Seeded(0x00C0_FFEE_2026_1114);
    let openai_mid = "T3BlbkFJ";
    let mut v: Vec<(&'static str, String)> = Vec::new();
    let mut add = |name: &'static str, line: String| v.push((name, line));
    add(
        "anthropic api key",
        format!(
            "ANTHROPIC_API_KEY={}-{}-{}AA",
            "sk-ant",
            "api03",
            r.take(URLSAFE, 93)
        ),
    );
    add(
        "openai project api key",
        format!(
            "OPENAI_API_KEY={}-{}{openai_mid}{}",
            "sk-proj",
            r.take(URLSAFE, 74),
            r.take(URLSAFE, 74)
        ),
    );
    add(
        "stripe live key",
        format!(
            "stripe.api_key = \"{}_{}_{}\"",
            "sk",
            "live",
            r.take(ALNUM, 99)
        ),
    );
    add(
        "stripe test key",
        format!("STRIPE_KEY={}_{}_{}", "sk", "test", r.take(ALNUM, 99)),
    );
    add(
        "stripe restricted test key",
        format!("key: {}_{}_{}", "rk", "test", r.take(ALNUM, 99)),
    );
    add(
        "stripe restricted live key",
        format!("key: {}_{}_{}", "rk", "live", r.take(ALNUM, 99)),
    );
    add(
        "openai admin api key",
        format!(
            "admin = \"{}-{}{openai_mid}{}\"",
            "sk-admin",
            r.take(URLSAFE, 58),
            r.take(URLSAFE, 58)
        ),
    );
    add(
        "openrouter api key",
        format!(
            "OPENROUTER_API_KEY={}-{}-{}",
            "sk-or",
            "v1",
            r.take(HEX, 64)
        ),
    );
    add(
        "openai api key",
        format!(
            "openai.api_key = \"{}-{}{openai_mid}{}\"",
            "sk",
            r.take(ALNUM, 20),
            r.take(ALNUM, 20)
        ),
    );
    add(
        "stripe webhook signing secret",
        format!("WEBHOOK_SECRET={}_{}", "whsec", r.take(ALNUM, 32)),
    );
    add(
        "xai api key",
        format!("XAI_API_KEY={}-{}", "xai", r.take(ALNUM, 80)),
    );
    add(
        "google api key",
        format!("apiKey: \"{}{}\"", "AIza", r.take(URLSAFE, 35)),
    );
    add(
        "google oauth access token",
        format!("token={}.{}", "ya29", r.take(URLSAFE, 120)),
    );
    add(
        "google oauth refresh token",
        format!("refresh={}/{}", "1/", r.take(URLSAFE, 64)),
    );
    add(
        "google oauth client secret",
        format!("client_secret={}-{}", "GOCSPX", r.take(URLSAFE, 28)),
    );
    add(
        "github personal access token",
        format!("GH_TOKEN={}_{}", "ghp", r.take(ALNUM, 36)),
    );
    add(
        "github oauth token",
        format!("token: {}_{}", "gho", r.take(ALNUM, 36)),
    );
    add(
        "github app server token",
        format!("token: {}_{}", "ghs", r.take(ALNUM, 36)),
    );
    add(
        "github app user token",
        format!("token: {}_{}", "ghu", r.take(ALNUM, 36)),
    );
    add(
        "github refresh token",
        format!("token: {}_{}", "ghr", r.take(ALNUM, 36)),
    );
    add(
        "github pat",
        format!(
            "GITHUB_TOKEN={}_{}_{}",
            "github_pat",
            r.take(ALNUM, 22),
            r.take(ALNUM, 59)
        ),
    );
    add(
        "gitlab personal access token",
        format!("GITLAB_TOKEN={}-{}", "glpat", r.take(URLSAFE, 20)),
    );
    add(
        "gitlab deploy token",
        format!("token {}-{}", "gldt", r.take(URLSAFE, 24)),
    );
    add(
        "gitlab runner authentication token",
        format!("token {}-{}", "glrt", r.take(URLSAFE, 24)),
    );
    add(
        "gitlab runner registration-created token",
        format!("token {}-{}", "glrtr", r.take(URLSAFE, 24)),
    );
    add(
        "gitlab oauth application secret",
        format!("secret {}-{}", "gloas", r.take(HEX, 64)),
    );
    add(
        "gitlab ci job token",
        format!("CI_JOB_TOKEN={}-{}", "glcbt", r.take(URLSAFE, 24)),
    );
    add(
        "gitlab trigger token",
        format!("token {}-{}", "glptt", r.take(HEX, 40)),
    );
    add(
        "gitlab feed token",
        format!("token {}-{}", "glft", r.take(URLSAFE, 24)),
    );
    add(
        "gitlab incoming mail token",
        format!("token {}-{}", "glimt", r.take(URLSAFE, 25)),
    );
    add(
        "gitlab agent for kubernetes token",
        format!("token {}-{}", "glagent", r.take(URLSAFE, 50)),
    );
    add(
        "gitlab workspace token",
        format!("token {}-{}", "glwt", r.take(URLSAFE, 24)),
    );
    add(
        "gitlab scim token",
        format!("token {}-{}", "glsoat", r.take(URLSAFE, 24)),
    );
    add(
        "gitlab feature flags client token",
        format!("token {}-{}", "glffct", r.take(URLSAFE, 24)),
    );
    add(
        "slack bot token",
        format!(
            "SLACK_BOT_TOKEN={}-{}-{}-{}",
            "xoxb",
            r.take("0123456789", 12),
            r.take("0123456789", 13),
            r.take(ALNUM, 24)
        ),
    );
    add(
        "slack user token",
        format!(
            "SLACK_TOKEN={}-{}-{}-{}-{}",
            "xoxp",
            r.take("0123456789", 12),
            r.take("0123456789", 12),
            r.take("0123456789", 13),
            r.take(HEX, 32)
        ),
    );
    add(
        "slack workspace token",
        format!(
            "token={}-{}-{}-{}-{}",
            "xoxa",
            "2",
            r.take("0123456789", 12),
            r.take("0123456789", 13),
            r.take(HEX, 32)
        ),
    );
    add(
        "slack app-level token",
        format!(
            "SLACK_APP_TOKEN={}-1-A{}-{}-{}",
            "xapp",
            r.take(UPPER_ALNUM, 10),
            r.take("0123456789", 13),
            r.take(HEX, 64)
        ),
    );
    add(
        "slack incoming webhook",
        format!(
            "url = \"https://{}.slack.com/services/T{}/B{}/{}\"",
            "hooks",
            r.take(UPPER_ALNUM, 10),
            r.take(UPPER_ALNUM, 10),
            r.take(ALNUM, 24)
        ),
    );
    add(
        "sendgrid api key",
        format!(
            "SENDGRID_API_KEY={}.{}.{}",
            "SG",
            r.take(URLSAFE, 22),
            r.take(URLSAFE, 43)
        ),
    );
    add(
        "shopify access token",
        format!("token = \"{}_{}\"", "shpat", r.take(HEX, 32)),
    );
    add(
        "shopify custom app token",
        format!("token = \"{}_{}\"", "shpca", r.take(HEX, 32)),
    );
    add(
        "shopify private app token",
        format!("token = \"{}_{}\"", "shppa", r.take(HEX, 32)),
    );
    add(
        "shopify shared secret",
        format!("secret = \"{}_{}\"", "shpss", r.take(HEX, 32)),
    );
    add(
        "aws access key id",
        format!("aws_access_key_id = {}{}", "AKIA", r.take(UPPER_ALNUM, 16)),
    );
    add(
        "aws temporary access key id",
        format!("aws_access_key_id = {}{}", "ASIA", r.take(UPPER_ALNUM, 16)),
    );
    add(
        "huggingface token",
        format!("HF_TOKEN={}_{}", "hf", r.take(ALNUM, 34)),
    );
    add(
        "npm access token",
        format!(
            "//registry.npmjs.org/:_authToken={}_{}",
            "npm",
            r.take(ALNUM, 36)
        ),
    );
    add(
        "pypi api token",
        format!(
            "password = {}-{}{}",
            "pypi",
            "AgEIcHlwaS5vcmc",
            r.take(URLSAFE, 120)
        ),
    );
    add(
        "digitalocean personal access token",
        format!("DO_TOKEN={}_v1_{}", "dop", r.take(HEX, 64)),
    );
    add(
        "digitalocean oauth access token",
        format!("token {}_v1_{}", "doo", r.take(HEX, 64)),
    );
    add(
        "digitalocean oauth refresh token",
        format!("token {}_v1_{}", "dor", r.take(HEX, 64)),
    );
    add(
        "replicate api token",
        format!("REPLICATE_API_TOKEN={}_{}", "r8", r.take(ALNUM, 37)),
    );
    add(
        "linear api key",
        format!("LINEAR_API_KEY={}_api_{}", "lin", r.take(ALNUM, 40)),
    );
    add(
        "linear oauth token",
        format!("token {}_oauth_{}", "lin", r.take(HEX, 64)),
    );
    add(
        "notion api token",
        format!(
            "NOTION_TOKEN={}_{}{}",
            "ntn",
            r.take("0123456789", 11),
            r.take(ALNUM, 35)
        ),
    );
    add(
        "private key block",
        format!("{} RSA PRIVATE KEY-----", "-----BEGIN"),
    );
    // Context shapes: no vendor prefix, found by where the value sits.
    add(
        "json web token",
        format!(
            "jwt = {}.{}.{}",
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
            "eyJzdWIiOiIxMjM0NTY3ODkwIiwiaWF0IjoxNTE2MjM5MDIyfQ",
            r.take(URLSAFE, 43)
        ),
    );
    add(
        "http authorization credential",
        format!("Authorization: Bearer {}", r.take(ALNUM, 40)),
    );
    add(
        "password in a url",
        format!(
            "remote = https://{}:{}@git.example.com/app.git",
            "deploy",
            r.take(ALNUM, 18)
        ),
    );
    add(
        "password on a command line",
        format!("mysql -u root --{} {}", "password", r.take(ALNUM, 18)),
    );
    add(
        "credential assigned to a named key",
        format!("aws_secret_access_key = \"{}\"", r.take(ALNUM, 40)),
    );
    // Shapes measured missing against Kingfisher and Betterleaks and added in
    // DevCouncil cbd51369. Appended, so the seeded values above keep theirs.
    add(
        "gitlab deploy token",
        format!("token {}-{}", "gldt", r.take(URLSAFE, 20)),
    );
    add(
        "slack refresh token",
        format!("refresh={}-1-{}", "xoxe", r.take(ALNUM, 140)),
    );
    add(
        "slack rotated bot token",
        format!("token={}.{}-1-{}", "xoxe", "xoxb", r.take(ALNUM, 140)),
    );
    add(
        "aws sts service bearer token id",
        format!("id={}{}", "ABIA", r.take(UPPER_ALNUM, 16)),
    );
    add(
        "aws context-specific credential id",
        format!("id={}{}", "ACCA", r.take(UPPER_ALNUM, 16)),
    );
    add(
        "aws access key id",
        format!("aws_access_key_id={}{}", "A3TX", r.take(UPPER_ALNUM, 16)),
    );
    add(
        "huggingface organization token",
        format!("HF_TOKEN={}_{}_{}", "api", "org", r.take(ALNUM, 34)),
    );
    // Batch 1 of the recall harness's ranked misses, added in DevCouncil
    // b9b2314d: formats missing for vendors the table already named.
    add(
        "aws bedrock short-term api key",
        format!(
            "AWS_BEARER_TOKEN_BEDROCK={}-api-key-{}{}",
            "bedrock",
            "YmVkcm9jay5hbWF6b25hd3MuY29t",
            r.take(ALNUM, 100)
        ),
    );
    add(
        "aws bedrock long-term api key",
        format!("key={}{}", "ABSK", r.take(ALNUM, 132)),
    );
    add(
        "google gemini api key",
        format!("GEMINI_API_KEY={}{}", "AQ.Ab8RN6", r.take(ALNUM, 44)),
    );
    add(
        "gitlab runner registration token",
        format!("registration-token: {}{}", "GR1348941", r.take(ALNUM, 20)),
    );
    add(
        "gitlab session cookie",
        format!("Cookie: {}={}", "_gitlab_session", r.take(HEX, 32)),
    );
    add(
        "gitlab incoming email token",
        format!(
            "reply_to = {}+acme-app-1234-{}-issue@gitlab.example.com",
            "incoming",
            r.take(ALNUM, 26)
        ),
    );
    add(
        "stripe prod key",
        format!("STRIPE_KEY={}_{}", "sk_prod", r.take(ALNUM, 24)),
    );
    add(
        "stripe restricted prod key",
        format!("STRIPE_KEY={}_{}", "rk_prod", r.take(ALNUM, 24)),
    );
    add(
        "slack legacy token",
        format!(
            "token={}-{}-{}-{}-{}",
            "xoxo",
            r.take("0123456789", 10),
            r.take("0123456789", 10),
            r.take("0123456789", 12),
            r.take(HEX, 32)
        ),
    );
    add(
        "slack legacy workspace token",
        format!("token={}-2-{}", "xoxr", r.take(ALNUM, 12)),
    );
    add(
        "slack session token",
        format!(
            "d={}-{}-{}-{}-{}",
            "xoxc",
            r.take("0123456789", 11),
            r.take("0123456789", 11),
            r.take("0123456789", 12),
            r.take(HEX, 64)
        ),
    );
    add(
        "slack session cookie",
        format!(
            "Cookie: d={}-{}%2F{}",
            "xoxd",
            r.take(ALNUM, 90),
            r.take(ALNUM, 20)
        ),
    );
    v
}

/// Lines no credential sits on.
pub(super) fn benign() -> Vec<String> {
    let mut r = Seeded(7);
    vec![
        format!("      \"integrity\": \"sha512-{}==\",", r.take(URLSAFE, 86)),
        format!("  resolved \"https://registry.yarnpkg.com/left-pad/-/left-pad-1.3.0.tgz#{}\"", r.take(HEX, 40)),
        format!("checksum = \"{}\"", r.take(HEX, 64)),
        format!("github.com/pkg/errors v0.9.1 h1:{}=", r.take(URLSAFE, 43)),
        format!("commit {}", r.take(HEX, 40)),
        "id: 123e4567-e89b-12d3-a456-426614174000".into(),
        format!("data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB{}", r.take(ALNUM, 60)),
        "function a(b,c){return b.map(function(d){return d+c}).filter(Boolean).join(\",\")}var e=a([1,2],3);".into(),
        "const taskRunner = new TaskRunner('task-runner-skeleton-disk-risk-assessment');".into(),
        "api_key = os.environ[\"OPENAI_API_KEY\"]".into(),
        "password: ${{ secrets.DB_PASSWORD }}".into(),
        "token = \"<YOUR_TOKEN_HERE>\"".into(),
        "export GITHUB_TOKEN=$(gh auth token)".into(),
        "Authorization: Bearer ${TOKEN}".into(),
        "let password = prompt_password(\"Password: \")?;".into(),
        "secret_key_base: <%= ENV[\"SECRET_KEY_BASE\"] %>".into(),
        "url = \"https://github.com/owner/repo.git\"".into(),
        "-----BEGIN CERTIFICATE-----".into(),
        "-----BEGIN PUBLIC KEY-----".into(),
        format!("sha256 = \"{}\"", r.take(HEX, 64)),
        "  \"version\": \"4.17.21\", \"resolved\": \"https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz\"".into(),
        format!("cache-key: {}-{}", r.take(LOWER, 8), r.take(HEX, 40)),
        "The scanner reads every added line and names the shape it found, never the value.".into(),
        "fn risk_score(task: &Task) -> u32 { task.disk_usage / 1024 }".into(),
        // Lines from this checkout the table used to flag (measured
        // 2026-11-14), fixed in DevCouncil cbd51369.
        "fn npm_scan_reports_unreadable_files() {".into(),
        "const MIN_TOKEN_REFRESH_INTERVAL: Duration = Duration::from_secs(30);".into(),
        "    const token = ++latest;".into(),
        "    log::warn!(\"leaked bearer token: {scrubbed}\");".into(),
        format!("let url = format!(\"https://{{}}:{{}}@{{host}}/{{path}}\", user, {});", "token"),
        // Near-misses of the DevCouncil b9b2314d shapes.
        format!("let loader = {}_fixture_loader_for_payments();", "sk_test"),
        format!("const view = {}_dashboardView;", "sk_live"),
        format!("reply_to = {}+acme-app-1234-<your-token>-issue@gitlab.example.com", "incoming"),
        format!("struct {}Config {{ region: String }}", "ABSK"),
    ]
}

#[test]
fn every_shape_the_table_claims_is_found_and_named() {
    let mut missed = Vec::new();
    for (expected, line) in planted() {
        let mut tally = Tally::default();
        tally.scan_line("planted", 1, line.as_bytes());
        match tally.hits.first() {
            Some(hit) if hit.rule == expected => {}
            Some(hit) => missed.push(format!("{expected}: named {}", hit.rule)),
            None => missed.push(format!("{expected}: not found")),
        }
    }
    assert!(
        missed.is_empty(),
        "recall gaps against the table's own shapes: {missed:#?}"
    );
}

#[test]
fn benign_lines_never_block_a_commit() {
    let mut flagged = Vec::new();
    for line in benign() {
        let mut tally = Tally::default();
        tally.scan_line("benign", 1, line.as_bytes());
        if let Some(hit) = tally.hits.first() {
            flagged.push(format!("{} on {line:?}", hit.rule));
        }
    }
    assert!(flagged.is_empty(), "false positives: {flagged:#?}");
}

/// Writes the planted and benign corpora, one line per file, to
/// `$GITPULSE_LEAK_CORPUS_DIR` for an oracle comparison. Ignored by default:
/// it is a measurement aid, and it writes credential-shaped (synthetic) values
/// to disk.
#[test]
#[ignore = "measurement aid: run with GITPULSE_LEAK_CORPUS_DIR set"]
fn dump_corpus_for_oracles() {
    let dir = std::path::PathBuf::from(
        std::env::var_os("GITPULSE_LEAK_CORPUS_DIR").expect("GITPULSE_LEAK_CORPUS_DIR"),
    );
    std::fs::create_dir_all(dir.join("planted")).unwrap();
    std::fs::create_dir_all(dir.join("benign")).unwrap();
    let mut manifest = String::new();
    for (n, (name, line)) in planted().iter().enumerate() {
        let file = format!("p{n:02}.txt");
        std::fs::write(dir.join("planted").join(&file), format!("{line}\n")).unwrap();
        manifest.push_str(&format!("{file}\t{name}\n"));
    }
    for (n, line) in benign().iter().enumerate() {
        std::fs::write(
            dir.join("benign").join(format!("b{n:02}.txt")),
            format!("{line}\n"),
        )
        .unwrap();
    }
    std::fs::write(dir.join("planted.tsv"), manifest).unwrap();
}
