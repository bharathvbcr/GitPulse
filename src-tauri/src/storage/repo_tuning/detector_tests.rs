use super::*;
use proptest::prelude::*;

fn entry(key: &str, value: &str, scope: ConfigScope) -> ConfigEntry {
    ConfigEntry {
        key: key.to_string(),
        value: value.to_string(),
        scope,
    }
}

fn header(version: u32, entries: u32) -> Vec<u8> {
    let mut bytes = b"DIRC".to_vec();
    bytes.extend_from_slice(&version.to_be_bytes());
    bytes.extend_from_slice(&entries.to_be_bytes());
    bytes
}

/// A well-formed-looking index: header, `entries` filler entries and a trailer.
fn index_bytes(version: u32, entries: u32, trailer: &[u8]) -> Vec<u8> {
    let mut bytes = header(version, entries);
    bytes.extend(std::iter::repeat_n(0xAB, entries as usize * 64));
    bytes.extend_from_slice(trailer);
    bytes
}

struct Repo {
    dir: tempfile::TempDir,
}

impl Repo {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git/objects/pack")).unwrap();
        Self { dir }
    }
    fn root(&self) -> &Path {
        self.dir.path()
    }
    fn git(&self) -> PathBuf {
        self.dir.path().join(".git")
    }
    fn report(&self, config: &[ConfigEntry]) -> RepoTuningReport {
        inspect_with(self.root(), &self.git(), &self.git(), None, Ok(config))
    }
}

#[test]
fn header_accepts_v2_v3_v4_and_reads_the_entry_count() {
    for version in [2, 3, 4] {
        assert_eq!(
            parse_index_header(&header(version, 41_000)),
            Ok(IndexHeader {
                version,
                entries: 41_000
            })
        );
    }
}

#[test]
fn header_refuses_what_is_not_a_known_index() {
    assert!(parse_index_header(b"").is_err());
    assert!(parse_index_header(&header(2, 1)[..11]).is_err());
    assert!(parse_index_header(&header(1, 1)).is_err());
    assert!(parse_index_header(&header(5, 1)).is_err());
    let mut wrong = header(2, 1);
    wrong[0] = b'X';
    assert!(parse_index_header(&wrong).is_err());
}

proptest! {
    /// Arbitrary bytes never panic the parser, and an `Ok` is exactly what the
    /// twelve bytes say.
    #[test]
    fn header_parser_is_total_and_exact(bytes in proptest::collection::vec(any::<u8>(), 0..40)) {
        match parse_index_header(&bytes) {
            Ok(h) => {
                prop_assert!(bytes.len() >= 12);
                prop_assert_eq!(&bytes[..4], b"DIRC");
                prop_assert!((2..=4).contains(&h.version));
                prop_assert_eq!(h.version, u32::from_be_bytes(bytes[4..8].try_into().unwrap()));
                prop_assert_eq!(h.entries, u32::from_be_bytes(bytes[8..12].try_into().unwrap()));
            }
            Err(why) => prop_assert!(!why.is_empty()),
        }
    }

    /// Any version in range with any count round-trips, with any tail after.
    #[test]
    fn header_round_trips(version in 2u32..=4, entries in any::<u32>(), tail in proptest::collection::vec(any::<u8>(), 0..64)) {
        let mut bytes = header(version, entries);
        bytes.extend(tail);
        prop_assert_eq!(parse_index_header(&bytes), Ok(IndexHeader { version, entries }));
    }

    /// A header with a mutated byte is either refused or still self-consistent.
    #[test]
    fn a_flipped_header_byte_never_yields_a_bogus_version(at in 0usize..12, bit in 0u8..8, version in 2u32..=4, entries in any::<u32>()) {
        let mut bytes = header(version, entries);
        bytes[at] ^= 1 << bit;
        if let Ok(h) = parse_index_header(&bytes) {
            prop_assert!((2..=4).contains(&h.version));
        }
    }
}

#[test]
fn index_reader_takes_each_version_and_both_trailers() {
    let repo = Repo::new();
    for version in [2u32, 3, 4] {
        fs::write(
            repo.git().join("index"),
            index_bytes(version, 3, &[0x11; 20]),
        )
        .unwrap();
        let index = read_index(&repo.git(), 20, true);
        assert_eq!(index.state, IndexReadState::Present);
        assert_eq!(index.version, Some(version));
        assert_eq!(index.entries, Some(3));
        assert_eq!(index.skip_hash, Some(false), "a real checksum");
    }
    fs::write(repo.git().join("index"), index_bytes(4, 3, &[0; 20])).unwrap();
    let zeroed = read_index(&repo.git(), 20, true);
    assert_eq!(
        zeroed.skip_hash,
        Some(true),
        "skipHash writes a zero trailer"
    );
    assert_eq!(zeroed.state, IndexReadState::Present);
    // sha256 is 32 bytes wide
    fs::write(repo.git().join("index"), index_bytes(2, 2, &[0; 32])).unwrap();
    assert_eq!(read_index(&repo.git(), 32, true).skip_hash, Some(true));
}

#[test]
fn index_reader_is_honest_about_what_it_cannot_read() {
    let repo = Repo::new();
    let missing = read_index(&repo.git(), 20, true);
    assert_eq!(missing.state, IndexReadState::Missing);
    assert_eq!(missing.entries, None);

    let index = repo.git().join("index");
    for (bytes, label) in [
        (b"".to_vec(), "empty"),
        (b"DIRC\0\0".to_vec(), "truncated header"),
        (header(9, 1), "unknown version"),
        (b"garbage garbage garbage".to_vec(), "bad signature"),
        (header(2, 1_000_000), "count the file cannot hold"),
    ] {
        fs::write(&index, bytes).unwrap();
        let got = read_index(&repo.git(), 20, true);
        assert_eq!(got.state, IndexReadState::Unknown, "{label}");
        assert!(got.note.is_some(), "{label}: unknown must say why");
        assert_eq!(got.entries, None, "{label}");
    }
    // A directory named `index` is not an index.
    fs::remove_file(&index).unwrap();
    fs::create_dir(&index).unwrap();
    assert_eq!(
        read_index(&repo.git(), 20, true).state,
        IndexReadState::Unknown
    );
}

#[test]
fn a_header_only_index_has_no_readable_trailer() {
    let repo = Repo::new();
    fs::write(repo.git().join("index"), header(2, 0)).unwrap();
    let index = read_index(&repo.git(), 20, true);
    assert_eq!(index.state, IndexReadState::Present);
    assert_eq!(index.skip_hash, None);
}

#[test]
fn split_index_and_sparse_index_make_the_count_inexact() {
    let repo = Repo::new();
    fs::write(repo.git().join("index"), index_bytes(2, 3, &[1; 20])).unwrap();
    let exact = repo.report(&[]);
    assert!(exact.index.entries_exact);
    for key in ["core.splitindex", "index.sparse"] {
        let report = repo.report(&[entry(key, "true", ConfigScope::Local)]);
        assert!(!report.index.entries_exact, "{key}");
        assert!(report.index.note.is_some());
    }
}

#[test]
fn packs_midx_and_loose_objects_are_counted_from_files() {
    let repo = Repo::new();
    let pack = repo.git().join("objects/pack");
    for i in 0..5 {
        fs::write(pack.join(format!("pack-{i}.pack")), b"").unwrap();
        fs::write(pack.join(format!("pack-{i}.idx")), b"").unwrap();
    }
    let none = repo.report(&[]);
    assert_eq!(none.objects.packs, Some(5), "only .pack files count");
    assert_eq!(none.midx, Some(false));
    fs::write(pack.join("multi-pack-index"), b"").unwrap();
    assert_eq!(repo.report(&[]).midx, Some(true));

    let seventeen = repo.git().join("objects/17");
    fs::create_dir_all(&seventeen).unwrap();
    for i in 0..30 {
        fs::write(seventeen.join(format!("{i:038x}")), b"").unwrap();
    }
    // Not 38 hex digits: a stray file is not an object.
    fs::write(seventeen.join("tmp_obj_123"), b"").unwrap();
    let loose = repo.report(&[]);
    assert_eq!(loose.objects.loose_objects, Some(30 * 256));
    assert!(!loose.objects.loose_exact);
    assert_eq!(loose.objects.gc_auto, Some(GC_AUTO_DEFAULT));
    assert_eq!(loose.objects.loose_over_gc_auto, Some(true));
}

#[test]
fn a_chain_of_incremental_midx_files_counts_as_a_midx() {
    let repo = Repo::new();
    let d = repo.git().join("objects/pack/multi-pack-index.d");
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("multi-pack-index-chain"), b"abc\n").unwrap();
    assert_eq!(repo.report(&[]).midx, Some(true));
}

#[test]
fn a_caller_supplied_count_replaces_the_estimate() {
    let repo = Repo::new();
    let report = inspect_with(
        repo.root(),
        &repo.git(),
        &repo.git(),
        Some(ObjectCounts {
            loose_objects: 12_000,
            packs: 61,
        }),
        Ok(&[entry("gc.auto", "10k", ConfigScope::Local)]),
    );
    assert_eq!(report.objects.packs, Some(61));
    assert_eq!(report.objects.loose_objects, Some(12_000));
    assert!(report.objects.loose_exact);
    assert_eq!(report.objects.gc_auto, Some(10 * 1024));
    assert_eq!(report.objects.loose_over_gc_auto, Some(true));
}

#[test]
fn gc_auto_zero_is_never_over_and_garbage_is_unknown() {
    let repo = Repo::new();
    let counts = Some(ObjectCounts {
        loose_objects: 1_000_000,
        packs: 1,
    });
    let zero = inspect_with(
        repo.root(),
        &repo.git(),
        &repo.git(),
        counts,
        Ok(&[entry("gc.auto", "0", ConfigScope::Global)]),
    );
    assert_eq!(zero.objects.gc_auto, Some(0));
    assert_eq!(zero.objects.loose_over_gc_auto, Some(false));
    let junk = inspect_with(
        repo.root(),
        &repo.git(),
        &repo.git(),
        counts,
        Ok(&[entry("gc.auto", "lots", ConfigScope::Global)]),
    );
    assert_eq!(junk.objects.gc_auto, None);
    assert_eq!(junk.objects.loose_over_gc_auto, None);
}

#[test]
fn an_unreadable_object_directory_is_unknown_not_zero() {
    let dir = tempfile::tempdir().unwrap();
    // No `.git/objects` at all is an empty store; a file where the pack dir
    // should be is unreadable.
    fs::create_dir_all(dir.path().join(".git/objects")).unwrap();
    fs::write(dir.path().join(".git/objects/pack"), b"x").unwrap();
    let git = dir.path().join(".git");
    let report = inspect_with(dir.path(), &git, &git, None, Ok(&[]));
    assert_eq!(report.objects.packs, None);
    assert_eq!(report.midx, None);
    assert!(report.notes.iter().any(|n| n.contains("objects/pack")));
}

#[test]
fn unreadable_config_makes_every_config_fact_unknown() {
    let repo = Repo::new();
    let report = inspect_with(repo.root(), &repo.git(), &repo.git(), None, Err("boom"));
    assert!(report.config_note.as_deref().unwrap().contains("boom"));
    assert_eq!(report.maintenance, MaintenanceState::default());
    assert_eq!(report.partial_clone, None);
    assert_eq!(report.sparse_checkout, None);
    assert_eq!(report.objects.gc_auto, None);
    assert_eq!(report.keys.len(), TUNING_KEYS.len());
    assert!(report
        .keys
        .iter()
        .all(|k| k.value.is_none() && k.origin.is_none()));
    // File facts are still read.
    assert_eq!(report.objects.packs, Some(0));
}

#[test]
fn maintenance_registration_reads_every_global_value_and_ignores_the_repo_list() {
    let repo = Repo::new();
    let me = fs::canonicalize(repo.root()).unwrap();
    let other = entry("maintenance.repo", "/somewhere/else", ConfigScope::Global);
    let strategy = entry("maintenance.strategy", "incremental", ConfigScope::Global);
    let auto = entry("maintenance.auto", "false", ConfigScope::Global);

    let none = repo.report(&[other.clone()]);
    assert_eq!(none.maintenance.registered, Some(false));

    let with_slash = format!("{}/", me.display());
    let yes = repo.report(&[
        other.clone(),
        entry("maintenance.repo", &with_slash, ConfigScope::Global),
        strategy,
        auto,
    ]);
    assert_eq!(yes.maintenance.registered, Some(true));
    assert_eq!(yes.maintenance.strategy.as_deref(), Some("incremental"));
    assert_eq!(yes.maintenance.auto.as_deref(), Some("false"));

    // git reads maintenance.repo from global and system config only.
    let local = repo.report(&[entry(
        "maintenance.repo",
        me.to_str().unwrap(),
        ConfigScope::Local,
    )]);
    assert_eq!(local.maintenance.registered, Some(false));
}

#[test]
fn a_linked_worktree_reads_its_index_privately_and_objects_commonly() {
    let main = Repo::new();
    let private = main.git().join("worktrees/wt");
    fs::create_dir_all(&private).unwrap();
    fs::write(private.join("index"), index_bytes(3, 7, &[0; 20])).unwrap();
    fs::write(main.git().join("index"), index_bytes(2, 99, &[1; 20])).unwrap();
    fs::write(main.git().join("objects/pack/pack-a.pack"), b"").unwrap();
    let wt_root = main.root().join("wt");
    fs::create_dir_all(&wt_root).unwrap();
    let report = inspect_with(&wt_root, &private, &main.git(), None, Ok(&[]));
    assert_eq!(report.index.entries, Some(7), "the worktree's own index");
    assert_eq!(report.index.version, Some(3));
    assert_eq!(report.index.skip_hash, Some(true));
    assert_eq!(
        report.objects.packs,
        Some(1),
        "objects live in the common dir"
    );
}

#[test]
fn keys_carry_value_and_origin_with_the_last_value_winning() {
    let repo = Repo::new();
    let report = repo.report(&[
        entry("core.fsmonitor", "false", ConfigScope::Global),
        entry("core.fsmonitor", "true", ConfigScope::Local),
        entry("index.version", "4", ConfigScope::System),
    ]);
    let fsmonitor = report.key("core.fsmonitor").unwrap();
    assert_eq!(fsmonitor.value.as_deref(), Some("true"));
    assert_eq!(fsmonitor.origin, Some(ConfigScope::Local));
    assert_eq!(
        report.key("index.version").unwrap().origin,
        Some(ConfigScope::System)
    );
    let unset = report.key("core.splitindex").unwrap();
    assert_eq!((unset.value.as_deref(), unset.origin), (None, None));
    assert_eq!(report.keys.len(), 7);
    assert!(report.config_note.is_none());
}

#[test]
fn partial_clone_and_sparse_checkout_are_read() {
    let repo = Repo::new();
    assert_eq!(repo.report(&[]).partial_clone, Some(false));
    assert_eq!(repo.report(&[]).sparse_checkout, Some(false));
    let partial = repo.report(&[entry("remote.origin.promisor", "true", ConfigScope::Local)]);
    assert_eq!(partial.partial_clone, Some(true));
    let sparse_cfg = repo.report(&[entry("core.sparsecheckout", "true", ConfigScope::Local)]);
    assert_eq!(sparse_cfg.sparse_checkout, Some(true));
    fs::create_dir_all(repo.git().join("info")).unwrap();
    fs::write(repo.git().join("info/sparse-checkout"), "/*\n").unwrap();
    assert_eq!(repo.report(&[]).sparse_checkout, Some(true));
}

#[test]
fn git_integers_take_a_suffix_and_refuse_the_rest() {
    assert_eq!(parse_git_unsigned("6700"), Some(6700));
    assert_eq!(parse_git_unsigned(" 2k "), Some(2048));
    assert_eq!(parse_git_unsigned("1m"), Some(1 << 20));
    assert_eq!(parse_git_unsigned("-1"), None);
    assert_eq!(parse_git_unsigned(""), None);
    assert_eq!(parse_git_unsigned("k"), None);
    assert_eq!(parse_git_unsigned("99999999999999999999g"), None);
}
