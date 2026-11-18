//! `set_modified` reaches a directory and a file, and the new time reads back.
//! On Windows the plain `File::open(..).set_times(..)` it replaces is
//! `Access is denied` for both; the Windows run is the check that matters, and
//! this pins the behaviour every host must share.

use super::set_modified;
use std::time::{Duration, SystemTime};

#[test]
fn a_directory_and_a_file_take_the_new_time() {
    let dir = tempfile::TempDir::new().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let file = sub.join("f");
    std::fs::write(&file, "x").unwrap();
    let when = SystemTime::now() - Duration::from_secs(90 * 86_400);
    for path in [&file, &sub] {
        set_modified(path, when).unwrap();
        let got = std::fs::metadata(path).unwrap().modified().unwrap();
        let skew = got.duration_since(when).unwrap_or_else(|e| e.duration());
        assert!(skew < Duration::from_secs(2), "{path:?}: {skew:?}");
    }
}

#[test]
fn a_missing_path_is_an_error_not_a_silent_skip() {
    let dir = tempfile::TempDir::new().unwrap();
    assert!(set_modified(&dir.path().join("absent"), SystemTime::now()).is_err());
}
