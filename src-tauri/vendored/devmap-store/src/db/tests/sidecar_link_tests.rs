use super::{sidecar_links, SidecarLinks};

#[test]
fn only_a_second_name_is_an_alias() {
    assert_eq!(sidecar_links(0), SidecarLinks::Unlinked);
    assert_eq!(sidecar_links(1), SidecarLinks::Single);
    assert_eq!(sidecar_links(2), SidecarLinks::Aliased);
    assert_eq!(sidecar_links(u64::MAX), SidecarLinks::Aliased);
}

/// The race is real, not hypothetical: a handle opened before the file is
/// deleted reports zero links, which the old `!= 1` test called "multiple".
#[cfg(unix)]
#[test]
fn a_sidecar_deleted_after_open_reports_zero_links() {
    let dir = std::env::temp_dir().join(format!(
        "devmap-sidecar-unlinked-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let wal = dir.join("index.sqlite-wal");
    std::fs::write(&wal, b"").unwrap();
    let handle = std::fs::File::open(&wal).unwrap();
    std::fs::remove_file(&wal).unwrap();
    let links = devmap_extract::safe_fs::file_link_count(&handle).unwrap();
    assert_eq!(links, 0);
    assert_eq!(sidecar_links(links), SidecarLinks::Unlinked);
    let _ = std::fs::remove_dir_all(&dir);
}
