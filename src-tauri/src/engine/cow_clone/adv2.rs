use super::*;
use tempfile::tempdir;

fn held(volume: u64) -> usize {
    RESERVED
        .lock()
        .unwrap()
        .iter()
        .filter(|(v, _)| *v == volume)
        .count()
}

#[test]
fn adv2_reservation_returns_to_zero_after_1000_mixed_outcomes() {
    const V: u64 = u64::MAX - 7;
    for i in 0..1000u64 {
        match i % 3 {
            0 => drop(Reservation::take(V, 100, 10)),
            1 => {
                let _ = std::panic::catch_unwind(|| {
                    let _r = Reservation::take(V, 100, 10);
                    panic!("copy panicked");
                });
            }
            _ => {
                let r = Reservation::take(V, 100, 10);
                let out: Result<(), String> = Err("failed".into());
                let _ = (&r, out);
            }
        }
    }
    assert_eq!(held(V), 0);
}

#[test]
fn adv2_reservation_never_blocks_and_starved_gets_zero() {
    const V: u64 = u64::MAX - 8;
    let a = Reservation::take(V, 10, 8);
    let b = Reservation::take(V, 10, 8);
    let c = Reservation::take(V, 10, 8);
    assert_eq!((a.bytes, b.bytes, c.bytes), (8, 2, 0));
    drop((a, b, c));
    assert_eq!(held(V), 0);
}

#[test]
fn adv2_two_volumes_and_nested_do_not_deadlock() {
    const X: u64 = u64::MAX - 9;
    const Y: u64 = u64::MAX - 10;
    let a = Reservation::take(X, 10, 5);
    let b = Reservation::take(Y, 10, 5);
    let c = Reservation::take(X, 10, 5);
    assert_eq!((a.bytes, b.bytes, c.bytes), (5, 5, 5));
}

#[test]
fn adv2_volume_of_missing_path_collapses_to_shared_key_zero() {
    let d = tempdir().unwrap();
    assert_eq!(volume_of(&d.path().join("nope")), 0);
    assert_ne!(volume_of(d.path()), 0);
}

fn leftovers(dir: &Path) -> usize {
    fs::read_dir(dir)
        .unwrap()
        .filter(|e| e.as_ref().unwrap().file_name() != "src")
        .count()
}

#[test]
fn adv2_one_huge_file_is_rechecked_only_after_it_is_fully_written() {
    use std::sync::atomic::AtomicUsize;
    let parent = tempdir().unwrap();
    let src = parent.path().join("src");
    fs::create_dir_all(&src).unwrap();
    let f = fs::File::create(src.join("huge")).unwrap();
    f.set_len(200 << 20).unwrap();
    drop(f);
    let calls = AtomicUsize::new(0);
    let recheck = || {
        calls.fetch_add(1, Ordering::SeqCst);
        Reading::Value(0)
    };
    let r = with_forced_byte_copy(|| {
        copy_dir_rechecked(
            &src,
            &parent.path().join("dst"),
            CowCaps::DEFAULT,
            CopyRules::default(),
            Some(&recheck),
        )
    });
    assert!(matches!(r, Err(CopyStop::BelowFloor { free: 0 })), "{r:?}");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "checked once, after the whole 200 MiB"
    );
    assert_eq!(leftovers(parent.path()), 0);
}

#[test]
fn adv2_unreadable_recheck_continues_silently() {
    let parent = tempdir().unwrap();
    let src = parent.path().join("src");
    fs::create_dir_all(&src).unwrap();
    for i in 0..3 {
        fs::write(src.join(format!("f{i}")), vec![1u8; 70 << 20]).unwrap();
    }
    let recheck = || Reading::Unavailable(Why::Failed);
    let r = with_forced_byte_copy(|| {
        copy_dir_rechecked(
            &src,
            &parent.path().join("dst"),
            CowCaps::DEFAULT,
            CopyRules::default(),
            Some(&recheck),
        )
    });
    assert!(r.is_ok(), "{r:?}");
}
