//! Adversarial checks on the git capability table: version parsing and the
//! floor.

use super::*;

fn line(text: &str) -> GitPreflight {
    assess(ToolProbe::Present(text.to_string()))
}

/// Deterministic xorshift, so a failing case replays.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[test]
fn adv_l1_assess_on_version_lines_names_the_right_features_and_never_panics() {
    for ok in [
        "git version 2.36.0",
        "git version 2.36",
        "git version 2.36.0.rc0",
        "git version 2.36.0-rc1",
        "git version 2.45.1.windows.1",
        "git version 2.39.3 (Apple Git-146)",
        "git version 3.0.0",
        "git version 10.1.0",
        "git version 2.36.0\r\n",
        "git version 2.36.0\n",
    ] {
        let answer = line(ok);
        assert_eq!(answer.status, GitPreflightStatus::Ok, "{ok:?}: {answer:?}");
        assert_eq!(answer.message, None, "{ok:?}");
    }
    let old = line("git version 2.35.99");
    assert_eq!(old.status, GitPreflightStatus::Outdated);
    let message = old.message.unwrap();
    assert!(message.contains("git worktree list -z"), "{message}");
    for absent in ["--diff-merges", "--pathspec", "--force-if-includes"] {
        assert!(!message.contains(absent), "{absent} in {message}");
    }
    for broken in [
        "",
        "git version",
        "git version ",
        "git version abc",
        "git version 99999999999999999999.1.0",
        "git version 4294967296.0.0",
        "git version 2.40.99999999999",
        "hub version 2.40.0",
    ] {
        let answer = line(broken);
        assert_eq!(answer.status, GitPreflightStatus::Broken, "{broken:?}");
        assert!(answer.message.unwrap().contains("2.36"), "{broken:?}");
    }
    assert_eq!(
        line("git version 4294967295.4294967295.4294967295").status,
        GitPreflightStatus::Ok
    );
}

/// A leading byte-order mark is not whitespace to `trim`, so a wrapper that
/// emits one makes a new git read as unrecognised. It fails closed, but the
/// message blames the git. Pinned as the current behaviour.
#[test]
fn adv_l1_a_leading_bom_reads_as_unrecognised_and_fails_closed() {
    let answer = line("\u{feff}git version 2.45.0");
    assert_eq!(answer.status, GitPreflightStatus::Broken);
    assert!(!supports(&answer, FORCE_IF_INCLUDES));
}

/// A component that overflows u32 is an unreadable version, not a zero: the
/// message must not invent "Git 2.0.0".
#[test]
fn adv_l1_an_overflowing_minor_is_silently_read_as_zero() {
    for text in [
        "git version 2.99999999999.0",
        "git version 2.36.99999999999",
        "git version 99999999999.0.0",
    ] {
        assert_eq!(parse_git_version(text), None, "{text}");
        let answer = line(text);
        assert_eq!(answer.status, GitPreflightStatus::Broken, "{text}");
        assert!(
            answer.message.unwrap().contains("unrecognised version"),
            "{text}"
        );
    }
    assert_eq!(parse_git_version("git version 2.36"), Some((2, 36, 0)));
}

/// The floor and the missing list agree with tuple order, including equal
/// major/minor with a different patch.
#[test]
fn adv_l1_the_floor_and_the_missing_list_agree_with_tuple_order_on_random_versions() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let floor = MIN_GIT_VERSION;
    let probes = [
        (2, 36, 0),
        (2, 36, 1),
        (2, 35, 99),
        (1, 99, 99),
        (3, 0, 0),
        (10, 1, 0),
        (2, 9, 0),
        (2, 100, 0),
    ];
    let randoms: Vec<(u32, u32, u32)> = (0..5000)
        .map(|_| {
            (
                rng.below(5) as u32,
                rng.below(60) as u32,
                rng.below(4) as u32,
            )
        })
        .collect();
    for version in probes.into_iter().chain(randoms) {
        let missing = missing_hard_features(version);
        assert_eq!(missing.is_empty(), version >= floor, "{version:?}");
        assert!(
            missing.windows(2).all(|pair| pair[0].min <= pair[1].min),
            "lowest first for {version:?}"
        );
        for row in GIT_FEATURES {
            let named = missing.iter().any(|m| m.name == row.name);
            assert_eq!(named, row.hard && version < row.min, "{version:?} {row:?}");
        }
        let text = format!("git version {}.{}.{}", version.0, version.1, version.2);
        assert_eq!(
            line(&text).status == GitPreflightStatus::Ok,
            version >= floor,
            "{text}"
        );
    }
}

/// `hard_floor_of` over random tables, against the plain max of the hard rows
/// (optional rows must not move it).
#[test]
fn adv_l1_the_floor_comparison_expression_is_a_lexicographic_max() {
    let mut rng = Rng(0xdead_beef_cafe_f00d);
    for _ in 0..20_000 {
        let n = 1 + rng.below(8) as usize;
        let table: Vec<GitFeature> = (0..n)
            .map(|_| GitFeature {
                name: "row",
                min: (
                    rng.below(3) as u32,
                    rng.below(3) as u32,
                    rng.below(3) as u32,
                ),
                hard: rng.below(4) != 0,
            })
            .collect();
        let expected = table
            .iter()
            .filter(|row| row.hard)
            .map(|row| row.min)
            .max()
            .unwrap_or((0, 0, 0));
        assert_eq!(hard_floor_of(&table), expected, "{table:?}");
    }
    assert_eq!(hard_floor_of(GIT_FEATURES), MIN_GIT_VERSION);
}

/// Every probe outcome that is not a readable passing version fails the
/// `supports` check, whatever the reason.
#[test]
fn adv_l1_every_unreadable_probe_fails_supports_with_a_distinct_status() {
    let deferred = classify_tool_probe(
        "git",
        Err("git version deferred under load after 2.000s: busy".to_string()),
    );
    let timeout = classify_tool_probe("git", Err("git timed out after 15s".to_string()));
    let statuses: Vec<GitPreflightStatus> = [deferred, timeout, ToolProbe::NotFound("x".into())]
        .into_iter()
        .map(|probe| {
            let answer = assess(probe);
            assert!(!supports(&answer, FORCE_IF_INCLUDES), "{answer:?}");
            answer.status
        })
        .collect();
    assert_eq!(
        statuses,
        [
            GitPreflightStatus::Unchecked,
            GitPreflightStatus::Broken,
            GitPreflightStatus::Missing
        ]
    );
}
