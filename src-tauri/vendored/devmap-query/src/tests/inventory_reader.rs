use super::*;

#[test]
#[cfg(unix)]
fn a_manifest_replaced_by_fifo_cannot_block_the_reader() {
    const PROBE: &str = "DEVMAP_INVENTORY_FIFO_PROBE";
    if let Some(root) = std::env::var_os(PROBE) {
        let mut refused = Vec::new();
        let mut unreadable = Vec::new();
        assert!(read_bounded(
            Path::new(&root),
            "package.json",
            &mut refused,
            &mut unreadable
        )
        .is_none());
        assert!(!unreadable.is_empty());
        return;
    }
    let root =
        std::env::temp_dir().join(format!("devmap-inventory-read-fifo-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let bounds = || Bounds {
        deadline: Duration::from_secs(2),
        stdout_cap: 4096,
        stderr_cap: 4096,
    };
    let made = run_bounded(
        std::process::Command::new("mkfifo").arg(root.join("package.json")),
        bounds(),
    )
    .unwrap();
    assert!(made.status.success());
    let outcome = run_bounded(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "inventory::reader_tests::a_manifest_replaced_by_fifo_cannot_block_the_reader",
            ])
            .env(PROBE, &root),
        bounds(),
    );
    std::fs::remove_dir_all(root).unwrap();
    let output = outcome.expect("manifest reader blocked opening a FIFO after marker discovery");
    assert!(output.status.success(), "{}", output.stderr_trimmed());
}
