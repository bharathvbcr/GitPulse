//! GitHub avatars for commit authors, resolved from commit oids.
//!
//! A commit's author email says nothing about a GitHub account (private
//! emails, noreply addresses, several emails for one person), but GitHub knows
//! which account authored each *pushed* commit. So the lookup is by oid: one
//! GraphQL `object(oid:)` alias per (email, oid) pair, batched, and the avatar
//! of the account GitHub links to the commit is what comes back.
//!
//! Every outcome is explicit. An email lands in exactly one of `avatars`
//! (linked account), `no_account` (GitHub answered, nobody linked) or
//! `unresolved` (never looked up, or GitHub could not answer for it): "could
//! not check" never renders as "no account".

use super::{
    bounded_error, capture_gh_graphql, discover_github_remote, gh_api_error_message,
    gh_unavailable_reason, probe_gh_cli, GitHubRepoRef,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// Most distinct emails looked up in one report; the rest are `unresolved`.
const MAX_EMAILS: usize = 150;
/// Oids tried per email; a second and third give a pushed commit a chance when
/// the newest one was never pushed.
const MAX_OIDS_PER_EMAIL: usize = 3;
/// Aliases (one per email/oid pair) in one GraphQL query.
const MAX_ALIASES_PER_QUERY: usize = 100;
/// GraphQL queries made for one report.
const MAX_QUERIES: usize = 5;
/// Longest one `gh api graphql` call may take.
const AVATAR_CALL_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_EMAIL_BYTES: usize = 320;
const MAX_URL_BYTES: usize = 2048;
const MAX_LOGIN_BYTES: usize = 100;
/// Avatar edge length asked of GitHub, in pixels.
const AVATAR_SIZE_PX: u32 = 64;

// The caps above are chosen together: every accepted pair fits a query, so
// planning never has to drop an accepted email for lack of a query slot.
const _: () = assert!(MAX_EMAILS * MAX_OIDS_PER_EMAIL <= MAX_QUERIES * MAX_ALIASES_PER_QUERY);

#[derive(Debug, Clone, Deserialize)]
pub struct AuthorAvatarQuery {
    pub email: String,
    pub oids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AuthorAvatar {
    pub email: String,
    pub login: String,
    pub avatar_url: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AuthorAvatarReport {
    /// false when the lookup could not run (no GitHub remote, gh missing, not authenticated, request failed).
    pub available: bool,
    pub avatars: Vec<AuthorAvatar>,
    /// Emails GitHub answered for whose commits have no linked GitHub account.
    pub no_account: Vec<String>,
    /// Emails not looked up or not answered: invalid input, past a cap, or every oid unknown to GitHub (unpushed).
    pub unresolved: Vec<String>,
    /// true when input exceeded a cap and some entries were not looked up.
    pub truncated: bool,
    pub error: Option<String>,
}

impl AuthorAvatarReport {
    fn unavailable(error: String, unresolved: Vec<String>, truncated: bool) -> Self {
        Self {
            available: false,
            avatars: Vec::new(),
            no_account: Vec::new(),
            unresolved,
            truncated,
            error: Some(error),
        }
    }

    /// The report for a lookup that never ran to an answer (the worker died or
    /// was cancelled).
    pub fn transport_failure(error: String) -> Self {
        Self::unavailable(error, Vec::new(), false)
    }
}

/// Looks up the GitHub avatar of each author. Never fails the caller: every
/// condition that prevents the lookup comes back as `available: false` with a
/// reason.
pub fn load_author_avatars(repo_path: &str, authors: Vec<AuthorAvatarQuery>) -> AuthorAvatarReport {
    let remote = match discover_github_remote(repo_path) {
        Ok(Some(remote)) => remote,
        Ok(None) => {
            return AuthorAvatarReport::unavailable(
                "No GitHub remote".to_string(),
                Vec::new(),
                false,
            )
        }
        Err(e) => {
            return AuthorAvatarReport::unavailable(
                bounded_error(e.into_message()),
                Vec::new(),
                false,
            )
        }
    };
    if let Some(reason) = gh_unavailable_reason(&probe_gh_cli()) {
        return AuthorAvatarReport::unavailable(reason, Vec::new(), false);
    }
    let plan = plan_lookup(authors);
    execute_plan(&plan, &remote.host, &|query| fetch_chunk(&remote, query))
}

/// One GraphQL call. `Ok` is a response body worth parsing (gh exits non-zero
/// when GraphQL reports errors yet still prints the body, whose message is the
/// specific reason); `Err` is a failure with nothing to parse.
fn fetch_chunk(remote: &GitHubRepoRef, query: &str) -> Result<String, String> {
    let out = capture_gh_graphql(remote, query, AVATAR_CALL_TIMEOUT)
        .map_err(|e| bounded_error(e.into_message()))?;
    let stdout = out.stdout_text();
    if out.success {
        return Ok(stdout);
    }
    let has_body = serde_json::from_str::<Value>(stdout.trim()).is_ok_and(|v| {
        v.get("data").is_some() || v.get("errors").is_some() || v.get("message").is_some()
    });
    if has_body {
        return Ok(stdout);
    }
    Err(classify_failure(gh_api_error_message(&out)))
}

/// Names the not-signed-in case, which the Health panel's wording does not
/// cover and which `gh` words several ways.
fn classify_failure(message: String) -> String {
    let lower = message.to_ascii_lowercase();
    if lower.contains("gh auth login")
        || lower.contains("not logged in")
        || lower.contains("http 401")
        || lower.contains("bad credentials")
    {
        return bounded_error(format!(
            "GitHub CLI is not authenticated (run `gh auth login`): {message}"
        ));
    }
    bounded_error(message)
}

// ---------------------------------------------------------------- planning

/// One accepted author: the email as first spelled and its validated oids.
#[derive(Debug, Clone, PartialEq)]
struct PlannedAuthor {
    email: String,
    oids: Vec<String>,
}

/// What will be asked of GitHub, and what was set aside while deciding.
#[derive(Debug, Default, PartialEq)]
struct LookupPlan {
    authors: Vec<PlannedAuthor>,
    unresolved: Vec<String>,
    truncated: bool,
}

fn valid_email(email: &str) -> bool {
    !email.is_empty() && email.len() <= MAX_EMAIL_BYTES && !email.chars().any(char::is_control)
}

/// A full-length git object id, lowercased, or `None`.
fn normalize_oid(oid: &str) -> Option<String> {
    let oid = oid.trim();
    let ok = (oid.len() == 40 || oid.len() == 64) && oid.bytes().all(|b| b.is_ascii_hexdigit());
    ok.then(|| oid.to_ascii_lowercase())
}

/// Validates and bounds the input. Emails are deduplicated case-insensitively
/// (the first spelling is the returned key, later oids are merged), at most
/// [`MAX_EMAILS`] are looked up, and each keeps at most [`MAX_OIDS_PER_EMAIL`]
/// oids. An email with nothing to look up is `unresolved`.
fn plan_lookup(queries: Vec<AuthorAvatarQuery>) -> LookupPlan {
    // (email, oids) in first-seen order, merged across duplicates.
    let mut merged: Vec<(String, Vec<String>)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for query in queries {
        let email = query.email.trim();
        // An empty or oversized key cannot be echoed back, so it is dropped.
        if email.is_empty() || email.len() > MAX_EMAIL_BYTES {
            continue;
        }
        let slot = *index.entry(email.to_ascii_lowercase()).or_insert_with(|| {
            merged.push((email.to_string(), Vec::new()));
            merged.len() - 1
        });
        // A control-character email is kept so it is reported, but never
        // given oids: it is `unresolved`, not looked up.
        if !valid_email(email) {
            continue;
        }
        let oids = &mut merged[slot].1;
        for oid in query.oids.iter().filter_map(|o| normalize_oid(o)) {
            if oids.len() < MAX_OIDS_PER_EMAIL && !oids.contains(&oid) {
                oids.push(oid);
            }
        }
    }
    let mut plan = LookupPlan::default();
    for (email, oids) in merged {
        if oids.is_empty() {
            plan.unresolved.push(email);
        } else if plan.authors.len() >= MAX_EMAILS {
            plan.truncated = true;
            plan.unresolved.push(email);
        } else {
            plan.authors.push(PlannedAuthor { email, oids });
        }
    }
    plan
}

/// The GraphQL query for `items` — `(alias number, oid)` pairs. Only an oid
/// that passes [`normalize_oid`] in its lowercase form is spliced into the
/// text; anything else refuses the whole query.
fn build_query(items: &[(usize, &str)]) -> Option<String> {
    let mut query =
        String::from("query($owner:String!,$name:String!){repository(owner:$owner,name:$name){");
    for (alias, oid) in items {
        let hex = normalize_oid(oid).filter(|n| n == oid)?;
        query.push_str(&format!(
            "c{alias}:object(oid:\"{hex}\"){{... on Commit{{author{{user{{login avatarUrl(size:{AVATAR_SIZE_PX})}}}}}}}}"
        ));
        query.push(' ');
    }
    query.push_str("}}");
    Some(query)
}

// ----------------------------------------------------------------- parsing

/// What GitHub said about one (email, oid) pair.
#[derive(Debug, Clone, PartialEq)]
enum AliasOutcome {
    /// The oid is unknown to GitHub (unpushed) or is not a commit.
    Missing,
    /// The commit exists and its author has no linked account (or the account's
    /// avatar could not be trusted).
    NoAccount,
    Account {
        login: String,
        avatar_url: String,
    },
}

/// Why GitHub's body is not an answer: its own message, else a fixed phrase.
fn graphql_error_text(value: &Value) -> Option<String> {
    let message = value
        .pointer("/errors/0/message")
        .and_then(Value::as_str)
        .or_else(|| value.get("message").and_then(Value::as_str))?;
    Some(bounded_error(message.to_string()))
}

/// Parses one query's response into an outcome per alias number.
fn parse_chunk(
    text: &str,
    aliases: &[usize],
    remote_host: &str,
) -> Result<HashMap<usize, AliasOutcome>, String> {
    let value: Value = serde_json::from_str(text.trim())
        .map_err(|error| format!("could not parse gh GraphQL output: {error}"))?;
    let repository = match value.pointer("/data/repository") {
        Some(repository) if repository.is_object() => repository,
        _ => {
            let reason = graphql_error_text(&value)
                .unwrap_or_else(|| "repository not found or not accessible".to_string());
            return Err(format!("GitHub could not read the repository: {reason}"));
        }
    };
    let mut outcomes = HashMap::new();
    for alias in aliases {
        let outcome = match repository.get(format!("c{alias}")) {
            None | Some(Value::Null) => AliasOutcome::Missing,
            Some(commit) => parse_commit(commit, remote_host),
        };
        outcomes.insert(*alias, outcome);
    }
    Ok(outcomes)
}

fn parse_commit(commit: &Value, remote_host: &str) -> AliasOutcome {
    // `... on Commit` yields `{}` for an object that is not a commit.
    let Some(author) = commit.get("author") else {
        return AliasOutcome::Missing;
    };
    let Some(user) = author.get("user").filter(|u| u.is_object()) else {
        return AliasOutcome::NoAccount;
    };
    let login = user.get("login").and_then(Value::as_str).unwrap_or("");
    let avatar_url = user.get("avatarUrl").and_then(Value::as_str).unwrap_or("");
    if login.is_empty()
        || login.len() > MAX_LOGIN_BYTES
        || login.chars().any(char::is_control)
        || !avatar_url_allowed(avatar_url, remote_host)
    {
        return AliasOutcome::NoAccount;
    }
    AliasOutcome::Account {
        login: login.to_string(),
        avatar_url: avatar_url.to_string(),
    }
}

/// True for an avatar URL the UI may load: `https`, no userinfo, and a host
/// that is GitHub's avatar CDN (`avatars.githubusercontent.com`) or the remote's
/// own host or a subdomain of it (GitHub Enterprise).
fn avatar_url_allowed(url: &str, remote_host: &str) -> bool {
    if url.len() > MAX_URL_BYTES || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return false;
    }
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, port),
        None => (authority, ""),
    };
    if !port.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let host = host.to_ascii_lowercase();
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return false;
    }
    let remote = remote_host.trim().to_ascii_lowercase();
    // Not every `*.githubusercontent.com`: `raw.` and friends serve arbitrary
    // repository content, so only the avatar CDN itself is admitted.
    host == "avatars.githubusercontent.com"
        || (!remote.is_empty() && (host == remote || host.ends_with(&format!(".{remote}"))))
}

// --------------------------------------------------------------- execution

/// Runs `plan` through `fetch` (one call per query) and assembles the report.
///
/// A failed query stops the run. If nothing had been answered the lookup is
/// unavailable; otherwise the report keeps what resolved, says the lookup was
/// partial, and an email with an unanswered oid and no avatar is `unresolved`
/// rather than `no_account` — the unanswered oid might have had one.
fn execute_plan(
    plan: &LookupPlan,
    remote_host: &str,
    fetch: &dyn Fn(&str) -> Result<String, String>,
) -> AuthorAvatarReport {
    // (author index, oid) per alias number.
    let flat: Vec<(usize, &str)> = plan
        .authors
        .iter()
        .enumerate()
        .flat_map(|(i, a)| a.oids.iter().map(move |oid| (i, oid.as_str())))
        .collect();
    let mut outcomes: HashMap<usize, AliasOutcome> = HashMap::new();
    let mut failed_aliases: HashSet<usize> = HashSet::new();
    let mut failure: Option<String> = None;
    for (chunk_no, chunk) in flat
        .chunks(MAX_ALIASES_PER_QUERY)
        .take(MAX_QUERIES)
        .enumerate()
    {
        let first = chunk_no * MAX_ALIASES_PER_QUERY;
        let aliases: Vec<usize> = (first..first + chunk.len()).collect();
        let items: Vec<(usize, &str)> = aliases
            .iter()
            .zip(chunk)
            .map(|(alias, (_, oid))| (*alias, *oid))
            .collect();
        let answered = build_query(&items)
            .ok_or_else(|| "refused to query an invalid object id".to_string())
            .and_then(|query| fetch(&query))
            .and_then(|text| parse_chunk(&text, &aliases, remote_host));
        match answered {
            Ok(chunk_outcomes) => outcomes.extend(chunk_outcomes),
            Err(error) => {
                failure = Some(error);
                failed_aliases.extend(first..flat.len());
                break;
            }
        }
    }

    let mut report = AuthorAvatarReport {
        available: true,
        avatars: Vec::new(),
        no_account: Vec::new(),
        unresolved: plan.unresolved.clone(),
        truncated: plan.truncated,
        error: None,
    };
    if let Some(error) = &failure {
        if outcomes.is_empty() {
            let everyone = plan
                .unresolved
                .iter()
                .cloned()
                .chain(plan.authors.iter().map(|a| a.email.clone()))
                .collect();
            return AuthorAvatarReport::unavailable(error.clone(), everyone, plan.truncated);
        }
        report.error = Some(format!("Avatar lookup was partial: {error}"));
    }

    let mut alias = 0;
    for author in &plan.authors {
        let mine = alias..alias + author.oids.len();
        alias += author.oids.len();
        let account = mine.clone().find_map(|a| match outcomes.get(&a) {
            Some(AliasOutcome::Account { login, avatar_url }) => {
                Some((login.clone(), avatar_url.clone()))
            }
            _ => None,
        });
        if let Some((login, avatar_url)) = account {
            report.avatars.push(AuthorAvatar {
                email: author.email.clone(),
                login,
                avatar_url,
            });
        } else if mine.clone().any(|a| failed_aliases.contains(&a)) {
            report.unresolved.push(author.email.clone());
        } else if mine
            .clone()
            .any(|a| outcomes.get(&a) == Some(&AliasOutcome::NoAccount))
        {
            report.no_account.push(author.email.clone());
        } else {
            report.unresolved.push(author.email.clone());
        }
    }
    report
}

#[cfg(test)]
mod avatars_tests {
    use super::*;
    use serde_json::json;
    use std::cell::{Cell, RefCell};

    const OID_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OID_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const OID_C: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const OID_D: &str = "dddddddddddddddddddddddddddddddddddddddd";
    const CDN: &str = "https://avatars.githubusercontent.com/u/1?s=64&v=4";

    fn q(email: &str, oids: &[&str]) -> AuthorAvatarQuery {
        AuthorAvatarQuery {
            email: email.to_string(),
            oids: oids.iter().map(|o| (*o).to_string()).collect(),
        }
    }

    fn account(login: &str, url: &str) -> Value {
        json!({"author": {"user": {"login": login, "avatarUrl": url}}})
    }

    fn body(repo: Value) -> String {
        json!({"data": {"repository": repo}}).to_string()
    }

    // ------------------------------------------------------------ planning

    #[test]
    fn dedupes_emails_case_insensitively_keeping_first_spelling_and_merging_oids() {
        let plan = plan_lookup(vec![q("Ann@X.io", &[OID_A]), q("ann@x.IO", &[OID_B])]);
        assert_eq!(plan.authors.len(), 1);
        assert_eq!(plan.authors[0].email, "Ann@X.io");
        assert_eq!(plan.authors[0].oids, vec![OID_A, OID_B]);
    }

    #[test]
    fn keeps_at_most_three_oids_per_email_and_drops_repeats() {
        let plan = plan_lookup(vec![q("a@x.io", &[OID_A, OID_A, OID_B, OID_C, OID_D])]);
        assert_eq!(plan.authors[0].oids, vec![OID_A, OID_B, OID_C]);
    }

    #[test]
    fn caps_distinct_emails_and_reports_the_rest_as_unresolved_and_truncated() {
        let queries: Vec<_> = (0..MAX_EMAILS + 2)
            .map(|i| q(&format!("u{i}@x.io"), &[OID_A]))
            .collect();
        let plan = plan_lookup(queries);
        assert_eq!(plan.authors.len(), MAX_EMAILS);
        assert!(plan.truncated);
        assert_eq!(plan.unresolved, vec!["u150@x.io", "u151@x.io"]);
    }

    #[test]
    fn exactly_the_cap_is_not_truncated() {
        let queries: Vec<_> = (0..MAX_EMAILS)
            .map(|i| q(&format!("u{i}@x.io"), &[OID_A]))
            .collect();
        let plan = plan_lookup(queries);
        assert!(!plan.truncated);
        assert!(plan.unresolved.is_empty());
    }

    #[test]
    fn oids_are_lowercased_and_must_be_40_or_64_hex() {
        let upper = OID_A.to_ascii_uppercase();
        let sha256 = "a".repeat(64);
        let plan = plan_lookup(vec![
            q("a@x.io", &[upper.as_str()]),
            q("b@x.io", &[sha256.as_str()]),
            q("c@x.io", &["a".repeat(41).as_str()]),
            q("d@x.io", &["zz".repeat(20).as_str()]),
            q("e@x.io", &[format!("{OID_A}\"){{}}").as_str()]),
        ]);
        assert_eq!(plan.authors[0].oids, vec![OID_A]);
        assert_eq!(plan.authors[1].oids, vec![sha256]);
        assert_eq!(plan.authors.len(), 2);
        assert_eq!(plan.unresolved, vec!["c@x.io", "d@x.io", "e@x.io"]);
    }

    #[test]
    fn invalid_emails_are_never_looked_up() {
        let long = format!("{}@x.io", "a".repeat(MAX_EMAIL_BYTES));
        let plan = plan_lookup(vec![
            q("", &[OID_A]),
            q("   ", &[OID_A]),
            q(&long, &[OID_A]),
            q("bad\u{7}@x.io", &[OID_A]),
            q("  ok@x.io  ", &[OID_A]),
        ]);
        assert_eq!(plan.authors.len(), 1);
        assert_eq!(plan.authors[0].email, "ok@x.io");
        assert_eq!(plan.unresolved, vec!["bad\u{7}@x.io"]);
    }

    #[test]
    fn the_caps_fit_the_query_budget() {
        let queries: Vec<_> = (0..MAX_EMAILS)
            .map(|i| q(&format!("u{i}@x.io"), &[OID_A, OID_B, OID_C]))
            .collect();
        let plan = plan_lookup(queries);
        let aliases: usize = plan.authors.iter().map(|a| a.oids.len()).sum();
        assert!(aliases.div_ceil(MAX_ALIASES_PER_QUERY) <= MAX_QUERIES);
    }

    // --------------------------------------------------------------- query

    #[test]
    fn query_splices_only_validated_hex() {
        let query = build_query(&[(0, OID_A), (7, OID_B)]).unwrap();
        assert!(query.contains(&format!("c0:object(oid:\"{OID_A}\")")));
        assert!(query.contains(&format!("c7:object(oid:\"{OID_B}\")")));
        assert!(query.contains("avatarUrl(size:64)"));
        assert!(query.starts_with(
            "query($owner:String!,$name:String!){repository(owner:$owner,name:$name){"
        ));
        assert_eq!(query.matches('"').count(), 4);
    }

    #[test]
    fn query_refuses_an_oid_that_is_not_lowercase_hex() {
        let injected = format!("{OID_A}\"){{ x }} c9:object(oid:\"{OID_B}");
        assert!(build_query(&[(0, injected.as_str())]).is_none());
        assert!(build_query(&[(0, OID_A.to_ascii_uppercase().as_str())]).is_none());
        assert!(build_query(&[(0, "abc")]).is_none());
    }

    #[test]
    fn graphql_argv_uses_raw_string_fields_and_pins_the_host() {
        let remote = GitHubRepoRef {
            host: "ghe.example.com".into(),
            owner: "o".into(),
            name: "r".into(),
        };
        let args = crate::github::gh_graphql_args(&remote, "query-text");
        assert_eq!(&args[..2], ["api", "graphql"]);
        assert!(args.windows(2).any(|w| w == ["-f", "owner=o"]));
        assert!(args.windows(2).any(|w| w == ["-f", "name=r"]));
        assert!(args.windows(2).any(|w| w == ["-f", "query=query-text"]));
        assert!(!args.iter().any(|a| a == "-F" || a == "--field"));
        assert!(args
            .windows(2)
            .any(|w| w == ["--hostname", "ghe.example.com"]));
    }

    // ------------------------------------------------------------- parsing

    fn parse(repo: Value, aliases: &[usize]) -> Result<HashMap<usize, AliasOutcome>, String> {
        parse_chunk(&body(repo), aliases, "github.com")
    }

    #[test]
    fn parses_an_account_a_missing_account_a_null_alias_and_a_non_commit() {
        let out = parse(
            json!({
                "c0": account("octo", CDN),
                "c1": {"author": {"user": null}},
                "c2": null,
                "c3": {},
            }),
            &[0, 1, 2, 3],
        )
        .unwrap();
        assert_eq!(
            out[&0],
            AliasOutcome::Account {
                login: "octo".into(),
                avatar_url: CDN.into()
            }
        );
        assert_eq!(out[&1], AliasOutcome::NoAccount);
        assert_eq!(out[&2], AliasOutcome::Missing);
        assert_eq!(out[&3], AliasOutcome::Missing);
    }

    #[test]
    fn a_null_repository_with_errors_is_an_error_with_githubs_message() {
        let text = json!({"data": {"repository": null}, "errors": [{"message": "Could not resolve to a Repository"}]})
            .to_string();
        let err = parse_chunk(&text, &[0], "github.com").unwrap_err();
        assert!(err.contains("Could not resolve to a Repository"), "{err}");
    }

    #[test]
    fn a_null_repository_without_errors_is_still_an_error() {
        assert!(parse(Value::Null, &[0]).is_err());
    }

    #[test]
    fn a_top_level_message_body_is_an_error() {
        let err = parse_chunk(r#"{"message":"Bad credentials"}"#, &[0], "github.com").unwrap_err();
        assert!(err.contains("Bad credentials"), "{err}");
    }

    #[test]
    fn malformed_json_is_an_error() {
        assert!(parse_chunk("<html>", &[0], "github.com").is_err());
    }

    #[test]
    fn an_untrusted_avatar_or_login_is_no_account() {
        let out = parse(
            json!({
                "c0": account("octo", "http://avatars.githubusercontent.com/u/1"),
                "c1": account("octo", "https://evil.example/u/1"),
                "c2": account("", CDN),
                "c3": account(&"l".repeat(MAX_LOGIN_BYTES + 1), CDN),
            }),
            &[0, 1, 2, 3],
        )
        .unwrap();
        for alias in 0..4 {
            assert_eq!(out[&alias], AliasOutcome::NoAccount, "alias {alias}");
        }
    }

    // ---------------------------------------------------------------- urls

    #[test]
    fn url_rules() {
        let ok = |u: &str| avatar_url_allowed(u, "github.com");
        assert!(ok(CDN));
        assert!(ok("https://avatars.githubusercontent.com/in/29110?v=4"));
        assert!(ok("https://AVATARS.githubusercontent.com:443/u/1"));
        assert!(!ok("http://avatars.githubusercontent.com/u/1"));
        assert!(!ok("//avatars.githubusercontent.com/u/1"));
        assert!(!ok("https://evil.com/u/1"));
        assert!(!ok(
            "https://raw.githubusercontent.com/evil/repo/main/a.png"
        ));
        assert!(!ok("https://githubusercontent.com.evil.com/u/1"));
        assert!(!ok("https://evilgithubusercontent.com/u/1"));
        assert!(!ok("https://evil.com@avatars.githubusercontent.com/u/1"));
        assert!(!ok("https://avatars.githubusercontent.com@evil.com/u/1"));
        assert!(!ok("https://evil.com\\@avatars.githubusercontent.com/u/1"));
        assert!(!ok("https://avatars.githubusercontent.com:evil/u/1"));
        assert!(!ok("https://avatars.githubusercontent.com/u/1 x"));
        assert!(!ok("https://avatars.githubusercontent.com/u/1\n"));
        assert!(!ok(""));
        let long = format!(
            "https://avatars.githubusercontent.com/{}",
            "a".repeat(MAX_URL_BYTES)
        );
        assert!(!ok(&long));
    }

    #[test]
    fn enterprise_host_and_its_subdomains_are_allowed_only_for_that_remote() {
        let ghe = "ghe.example.com";
        assert!(avatar_url_allowed(
            "https://ghe.example.com/avatars/u/1",
            ghe
        ));
        assert!(avatar_url_allowed(
            "https://avatars.ghe.example.com/u/1",
            ghe
        ));
        assert!(!avatar_url_allowed(
            "https://ghe.example.com/avatars/u/1",
            "github.com"
        ));
        assert!(!avatar_url_allowed("https://evilghe.example.com/u/1", ghe));
        assert!(!avatar_url_allowed(
            "https://ghe.example.com.evil.com/u/1",
            ghe
        ));
    }

    // ----------------------------------------------------------- execution

    fn run(
        plan: &LookupPlan,
        fetch: impl Fn(&str) -> Result<String, String>,
    ) -> AuthorAvatarReport {
        execute_plan(plan, "github.com", &fetch)
    }

    #[test]
    fn the_first_oid_with_an_account_wins_and_a_later_one_covers_an_unpushed_first() {
        let plan = plan_lookup(vec![q("a@x.io", &[OID_A, OID_B])]);
        let report = run(&plan, |_| {
            Ok(body(json!({"c0": null, "c1": account("octo", CDN)})))
        });
        assert!(report.available);
        assert_eq!(
            report.avatars,
            vec![AuthorAvatar {
                email: "a@x.io".into(),
                login: "octo".into(),
                avatar_url: CDN.into()
            }]
        );
        assert!(report.no_account.is_empty() && report.unresolved.is_empty());
        assert_eq!(report.error, None);
    }

    #[test]
    fn splits_emails_into_avatar_no_account_and_unresolved() {
        let plan = plan_lookup(vec![
            q("has@x.io", &[OID_A]),
            q("none@x.io", &[OID_B]),
            q("unpushed@x.io", &[OID_C]),
            q("noshape@x.io", &["short"]),
        ]);
        let report = run(&plan, |_| {
            Ok(body(json!({
                "c0": account("octo", CDN),
                "c1": {"author": {"user": null}},
                "c2": null,
            })))
        });
        assert_eq!(report.avatars.len(), 1);
        assert_eq!(report.no_account, vec!["none@x.io"]);
        assert_eq!(report.unresolved, vec!["noshape@x.io", "unpushed@x.io"]);
    }

    #[test]
    fn a_commit_without_an_account_beside_an_unpushed_one_is_no_account() {
        let plan = plan_lookup(vec![q("a@x.io", &[OID_A, OID_B])]);
        let report = run(&plan, |_| {
            Ok(body(json!({"c0": {"author": {"user": null}}, "c1": null})))
        });
        assert_eq!(report.no_account, vec!["a@x.io"]);
    }

    #[test]
    fn splits_into_queries_of_at_most_100_aliases() {
        // 34 emails x 3 oids = 102 aliases -> two queries.
        let queries: Vec<_> = (0..34)
            .map(|i| q(&format!("u{i}@x.io"), &[OID_A, OID_B, OID_C]))
            .collect();
        let plan = plan_lookup(queries);
        let seen = RefCell::new(Vec::new());
        let report = run(&plan, |query| {
            seen.borrow_mut().push(query.matches(":object(").count());
            Ok(body(json!({})))
        });
        assert_eq!(*seen.borrow(), vec![100, 2]);
        assert!(report.available);
        assert_eq!(report.unresolved.len(), 34);
    }

    #[test]
    fn a_later_chunk_failing_keeps_earlier_answers_and_says_partial() {
        // 34 emails x 3 oids: the first 33 authors' aliases fit query one (99),
        // author 33's first alias is in query one and its other two in two.
        let queries: Vec<_> = (0..34)
            .map(|i| q(&format!("u{i}@x.io"), &[OID_A, OID_B, OID_C]))
            .collect();
        let plan = plan_lookup(queries);
        let calls = Cell::new(0);
        let report = run(&plan, |_| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                // alias 0 has an account; alias 99 (u33's first oid) has none.
                Ok(body(json!({
                    "c0": account("octo", CDN),
                    "c99": {"author": {"user": null}},
                })))
            } else {
                Err("boom".to_string())
            }
        });
        assert!(report.available);
        assert_eq!(report.avatars.len(), 1);
        assert_eq!(report.avatars[0].email, "u0@x.io");
        // u33's later oids were never answered: not "no account".
        assert!(report.unresolved.contains(&"u33@x.io".to_string()));
        assert!(!report.no_account.contains(&"u33@x.io".to_string()));
        let error = report.error.unwrap();
        assert!(
            error.contains("partial") && error.contains("boom"),
            "{error}"
        );
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn the_first_chunk_failing_is_unavailable_and_lists_everyone_unresolved() {
        let plan = plan_lookup(vec![q("a@x.io", &[OID_A]), q("b@x.io", &["bad"])]);
        let report = run(
            &plan,
            |_| Err("GitHub CLI is not authenticated".to_string()),
        );
        assert!(!report.available);
        assert!(report.avatars.is_empty() && report.no_account.is_empty());
        assert_eq!(report.unresolved, vec!["b@x.io", "a@x.io"]);
        assert!(report.error.unwrap().contains("not authenticated"));
    }

    #[test]
    fn a_response_that_is_not_json_is_unavailable_not_empty_success() {
        let plan = plan_lookup(vec![q("a@x.io", &[OID_A])]);
        let report = run(&plan, |_| Ok("<html>".to_string()));
        assert!(!report.available);
        assert!(report.error.is_some());
    }

    #[test]
    fn an_empty_plan_makes_no_call_and_is_available() {
        let plan = plan_lookup(vec![q("a@x.io", &[])]);
        let report = run(&plan, |_| panic!("no query is needed"));
        assert!(report.available);
        assert_eq!(report.unresolved, vec!["a@x.io"]);
    }

    #[test]
    fn truncation_from_planning_is_carried_into_the_report() {
        let queries: Vec<_> = (0..MAX_EMAILS + 1)
            .map(|i| q(&format!("u{i}@x.io"), &[OID_A]))
            .collect();
        let plan = plan_lookup(queries);
        let report = run(&plan, |_| Ok(body(json!({}))));
        assert!(report.truncated);
    }

    #[test]
    fn auth_failures_are_named() {
        let text =
            classify_failure("To get started with GitHub CLI, please run:  gh auth login".into());
        assert!(
            text.starts_with("GitHub CLI is not authenticated"),
            "{text}"
        );
        assert_eq!(classify_failure("HTTP 500".into()), "HTTP 500");
    }

    /// The whole live path — trust gate, remote discovery, `gh` probe, one real
    /// GraphQL call, parsing — against a pushed commit of a real repository.
    /// Opt-in: it needs network and a signed-in `gh`. Set
    /// `GITPULSE_AVATAR_LIVE_REPO` to a trusted checkout with a GitHub remote and
    /// `GITPULSE_AVATAR_LIVE_OID` to a commit of it that GitHub has. The test
    /// process does not see `gh`'s stored login, so pass
    /// `GH_TOKEN="$(gh auth token)"` as well.
    #[test]
    #[ignore = "network: needs gh signed in and GITPULSE_AVATAR_LIVE_REPO/OID"]
    fn live_lookup_resolves_a_pushed_commit() {
        let (Ok(repo), Ok(oid)) = (
            std::env::var("GITPULSE_AVATAR_LIVE_REPO"),
            std::env::var("GITPULSE_AVATAR_LIVE_OID"),
        ) else {
            panic!("set GITPULSE_AVATAR_LIVE_REPO and GITPULSE_AVATAR_LIVE_OID");
        };
        // Session-only approval through the production grant path; the
        // command itself must still pass the trust gate.
        crate::test_support::trust_repo(std::path::Path::new(&repo));
        let report = load_author_avatars(&repo, vec![q("live@example.invalid", &[&oid])]);
        assert!(report.available, "{report:?}");
        assert_eq!(report.avatars.len(), 1, "{report:?}");
        assert!(
            report.avatars[0]
                .avatar_url
                .starts_with("https://avatars.githubusercontent.com/"),
            "{report:?}"
        );
    }
}
