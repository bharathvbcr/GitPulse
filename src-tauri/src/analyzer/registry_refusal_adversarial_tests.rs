//! Adversarial review of the repo-config refusal (W8-L3). Each test states the
//! npm/bun behaviour it relies on; a failing test is a way to tell the scanner
//! about a registry, credential or proxy that `repo_config_refusal` misses.
//!
//! npm config sources: https://docs.npmjs.com/cli/configuring-npm/npmrc
//! (per-project `.npmrc` sits next to the project's real package.json, i.e.
//! the workspace root when the package is a workspace). `.npmrc` is parsed by
//! the `ini` package, which splits on `/[\r\n]+/` and unquotes keys.

use super::*;
use std::fs;

fn repo_with(files: &[(&str, &[u8])]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (rel, bytes) in files {
        let path = dir.path().join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, bytes).expect("write");
    }
    // The scan canonicalises the repo root (macOS /var -> /private/var).
    dir
}

fn refusal(dir: &tempfile::TempDir, rel_dir: &str, bun: bool) -> Option<(&'static str, String)> {
    let root = dir.path().canonicalize().expect("canonical");
    repo_config_refusal(&root, rel_dir, bun)
}

fn assert_refused(npmrc: &str) {
    let dir = repo_with(&[(".npmrc", npmrc.as_bytes())]);
    assert!(
        refusal(&dir, "", false).is_some(),
        "npm would honour this .npmrc but the scan ran: {npmrc:?}"
    );
}

// ---- held (positive controls) ---------------------------------------------

#[test]
fn control_plain_registry_forms_are_refused() {
    for text in [
        "registry=http://evil.test/\n",
        "registry = http://evil.test/\n",
        "REGISTRY=http://evil.test/\n",
        "@acme:registry=http://evil.test/\n",
        "//evil.test/:_authToken=abc\n",
        "_auth=abc\n",
        "registry=http://evil.test/\r\n",
        "https-proxy=http://evil.test:3128\n",
        "proxy=http://evil.test:3128\n",
        "[section]\nregistry=http://evil.test/\n",
        "registry=\"http://evil.test/\"\n",
    ] {
        assert_refused(text);
    }
}

#[test]
fn control_comment_only_lines_do_not_refuse() {
    let dir = repo_with(&[(
        ".npmrc",
        b"; registry=http://evil.test/\n# proxy=x\nfund=false\n",
    )]);
    assert!(refusal(&dir, "", false).is_none());
}

// ---- attacks ---------------------------------------------------------------

/// `ini` splits lines on `[\r\n]+`, so a lone CR separates entries. Rust's
/// `str::lines` does not split on a lone `\r`.
#[test]
fn lone_cr_line_separator_hides_registry() {
    assert_refused("fund=false\rregistry=http://evil.test/\n");
}

/// `ini` unquotes keys (`unsafe(match[1])`), so `"registry"=...` sets registry.
#[test]
fn quoted_key_registry() {
    assert_refused("\"registry\"=http://evil.test/\n");
}

/// Verified against npm 11.20.0 (`npm config get proxy --userconfig`): a UTF-8
/// BOM before the first key does not stop `proxy` taking effect. The scan's
/// exact-match keys (`proxy`, `https-proxy`, `username`) see `\u{feff}proxy`.
#[test]
fn bom_prefixed_first_key_proxy() {
    assert_refused("\u{feff}proxy=http://evil.test:3128\n");
    assert_refused("\u{feff}https-proxy=http://evil.test:3128\n");
    assert_refused("\u{feff}username=u\n");
}

#[test]
fn quoted_key_proxy_and_https_proxy() {
    assert_refused("\"https-proxy\"=http://evil.test:3128\n");
    assert_refused("\"proxy\"=http://evil.test:3128\n");
}

#[test]
fn single_quoted_key_proxy() {
    assert_refused("'proxy'=http://evil.test:3128\n");
}

/// npm's project `.npmrc` is read from the workspace root, which can be an
/// intermediate directory between the repo root and the scanned package.
#[test]
fn workspace_root_between_package_and_repo_root() {
    let dir = repo_with(&[
        ("apps/package.json", br#"{"workspaces":["web"]}"#),
        ("apps/.npmrc", b"registry=http://evil.test/\n"),
        ("apps/web/package.json", b"{}"),
    ]);
    assert!(
        refusal(&dir, "apps/web", false).is_some(),
        "an intermediate workspace root's .npmrc was never read"
    );
}

#[test]
fn deeply_nested_parent_npmrc() {
    let dir = repo_with(&[
        ("a/.npmrc", b"registry=http://evil.test/\n"),
        ("a/b/c/package.json", b"{}"),
    ]);
    assert!(refusal(&dir, "a/b/c", false).is_some());
}

/// TLS trust and client-certificate keys redirect where a credential-free
/// request is accepted (MITM) or attach a client identity; the owner's decision
/// named registry/token/proxy only, so these are reported as a policy gap.
#[test]
fn tls_trust_keys_policy_gap() {
    for text in [
        "strict-ssl=false\n",
        "cafile=/tmp/evil.pem\n",
        "ca=\"-----BEGIN CERTIFICATE-----\"\n",
        "//registry.npmjs.org/:certfile=/tmp/c.pem\n",
        "noproxy=registry.npmjs.org\n",
    ] {
        assert_refused(text);
    }
}

#[test]
fn tokenhelper_and_email_style_credentials() {
    assert_refused("//registry.npmjs.org/:tokenHelper=/tmp/evil.sh\n");
}

#[test]
fn oversized_npmrc_is_refused_not_ignored() {
    let big = vec![b'#'; (MAX_MANIFEST_BYTES as usize) + 10];
    let dir = repo_with(&[(".npmrc", &big)]);
    assert!(refusal(&dir, "", false).is_some());
}

#[cfg(unix)]
#[test]
fn symlinked_npmrc_to_outside_is_refused() {
    let outside = tempfile::tempdir().expect("outside");
    fs::write(outside.path().join("rc"), "registry=http://evil.test/\n").expect("rc");
    let dir = repo_with(&[("package.json", b"{}")]);
    std::os::unix::fs::symlink(outside.path().join("rc"), dir.path().join(".npmrc")).expect("ln");
    assert!(refusal(&dir, "", false).is_some());
}

#[cfg(unix)]
#[test]
fn dangling_symlink_npmrc_is_refused_or_harmless() {
    let dir = repo_with(&[("package.json", b"{}")]);
    std::os::unix::fs::symlink("nowhere", dir.path().join(".npmrc")).expect("ln");
    // npm cannot read it either, so either answer is defensible; it must not panic.
    let _ = refusal(&dir, "", false);
}

#[cfg(unix)]
#[test]
fn fifo_npmrc_does_not_hang_and_is_refused() {
    let dir = repo_with(&[("package.json", b"{}")]);
    let fifo = dir.path().join(".npmrc");
    let c = std::ffi::CString::new(fifo.to_str().expect("utf8")).expect("cstr");
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let root = dir.path().canonicalize().expect("canonical");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(repo_config_refusal(&root, "", false));
    });
    let got = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("repo_config_refusal hung on a FIFO .npmrc");
    assert!(got.is_some(), "unreadable config must refuse");
}

#[cfg(unix)]
#[test]
fn unreadable_npmrc_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let dir = repo_with(&[(".npmrc", b"registry=http://evil.test/\n")]);
    fs::set_permissions(dir.path().join(".npmrc"), fs::Permissions::from_mode(0o000))
        .expect("chmod");
    let got = refusal(&dir, "", false);
    fs::set_permissions(dir.path().join(".npmrc"), fs::Permissions::from_mode(0o600))
        .expect("chmod");
    assert!(got.is_some(), "an unreadable .npmrc was treated as empty");
}

// ---- bun -------------------------------------------------------------------

#[test]
fn bun_reads_npmrc_too() {
    let dir = repo_with(&[(".npmrc", b"registry=http://evil.test/\n")]);
    assert!(refusal(&dir, "", true).is_some());
}

#[test]
fn bunfig_forms_held() {
    for text in [
        "[install]\nregistry = \"http://evil.test/\"\n",
        "[install.scopes]\n\"@a\" = \"http://evil.test/\"\n",
        "[install.registry]\nurl = \"http://evil.test/\"\n",
        "[install]\nregistry = { url = \"http://evil.test/\", token = \"t\" }\n",
        "[install.scopes.\"@a\"]\ntoken = \"t\"\n",
    ] {
        let dir = repo_with(&[("bunfig.toml", text.as_bytes())]);
        assert!(refusal(&dir, "", true).is_some(), "{text:?}");
    }
}

/// TOML allows whitespace around the dots of a dotted table header.
#[test]
fn bunfig_header_with_spaces_around_dots() {
    for text in [
        "[ install . scopes ]\n\"@a\" = \"http://evil.test/\"\n",
        "[install . registry]\nurl = \"http://evil.test/\"\n",
    ] {
        let dir = repo_with(&[("bunfig.toml", text.as_bytes())]);
        assert!(refusal(&dir, "", true).is_some(), "{text:?}");
    }
}

/// TOML dotted keys at the top level are the same table as `[install]`.
#[test]
fn bunfig_top_level_dotted_keys() {
    for text in [
        "install.registry = \"http://evil.test/\"\n",
        "install.scopes.\"@a\" = \"http://evil.test/\"\n",
        "install.scopes = { \"@a\" = \"http://evil.test/\" }\n",
    ] {
        let dir = repo_with(&[("bunfig.toml", text.as_bytes())]);
        assert!(refusal(&dir, "", true).is_some(), "{text:?}");
    }
}

#[test]
fn bunfig_in_package_dir_and_root() {
    let dir = repo_with(&[
        (
            "pkg/bunfig.toml",
            b"[install]\nregistry = \"http://evil.test/\"\n",
        ),
        ("pkg/package.json", b"{}"),
    ]);
    assert!(refusal(&dir, "pkg", true).is_some());
}

// ---- regressions -----------------------------------------------------------

#[test]
fn no_config_means_no_refusal_for_npm_and_bun() {
    let dir = repo_with(&[("package.json", b"{}")]);
    assert!(refusal(&dir, "", false).is_none());
    assert!(refusal(&dir, "", true).is_none());
}

#[test]
fn benign_npmrc_does_not_refuse() {
    let dir = repo_with(&[(
        ".npmrc",
        b"fund=false\naudit-level=high\nengine-strict=true\n",
    )]);
    assert!(refusal(&dir, "", false).is_none());
}
