//! F-10: captured git output that is valid UTF-8 is decoded in place.
//!
//! Counts the bytes the calling thread allocates, so a test can say whether a
//! decode copied a buffer it had no need to. The counting allocator belongs to
//! this test binary alone; the library's own tests run on the default one.
//!
//! Covers the public text entries. `git_text_shared` and
//! `git_text_capped_shared` are crate-private (they are reached through
//! `ref_cache`) and are not measured here; they decode through the same
//! `text_from_bytes` as the entries below, which is checked by reading, and the
//! helper itself by `decoding_valid_captured_output_does_not_copy_the_buffer`
//! in the library's tests.

use gitpulse_lib::engine::git_cli::{
    git_bytes_capped, git_text, git_text_capped, git_text_partial, git_text_with_timeout,
    MAX_OUTPUT_BYTES,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::Duration;

mod common;

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
}

fn note(bytes: usize) {
    let _ = ON.try_with(|on| {
        if on.get() {
            let _ = BYTES.try_with(|total| total.set(total.get() + bytes));
        }
    });
}

struct Counting;

// SAFETY: every method forwards to `System` unchanged; the counting touches
// only const-initialised thread-locals and never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// What `body` returns and the bytes the calling thread allocated meanwhile.
fn allocated_by<T>(body: impl FnOnce() -> T) -> (T, usize) {
    BYTES.with(|total| total.set(0));
    ON.with(|on| on.set(true));
    let out = body();
    ON.with(|on| on.set(false));
    (out, BYTES.with(Cell::get))
}

/// Through every public entry that returns text, the call allocates no more than
/// the call that returns the bytes, give or take a fraction of the payload. A
/// decode that copies the buffer (`from_utf8_lossy(..).into_owned()`) costs one
/// whole payload more, which is what this measures.
#[cfg(unix)]
#[test]
fn text_entries_allocate_no_second_copy_of_the_output() {
    const PAYLOAD: usize = 6 * 1024 * 1024;
    common::unmetered_spawns();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path();
    common::run_git(path, &["init", "-b", "main"]);
    common::trust_repo(path);
    std::fs::write(
        path.join("big.txt"),
        "a line of ordinary text\n".repeat(PAYLOAD / 24),
    )
    .unwrap();
    let oid = git_text(path, &["hash-object", "-w", "big.txt"])
        .unwrap()
        .trim()
        .to_string();
    let args = ["cat-file", "blob", oid.as_str()];
    let slack = PAYLOAD / 3;

    // The runner's own cost for this output, with no decode.
    let (bytes, baseline) = allocated_by(|| git_bytes_capped(path, &args, MAX_OUTPUT_BYTES).unwrap());
    assert!(
        bytes.0.len() >= PAYLOAD - 24,
        "the fixture printed {} bytes",
        bytes.0.len()
    );
    drop(bytes);

    let routes: [(&str, usize); 4] = [
        ("git_text", allocated_by(|| git_text(path, &args).unwrap().len()).1),
        (
            "git_text_capped",
            allocated_by(|| git_text_capped(path, &args, MAX_OUTPUT_BYTES).unwrap().0.len()).1,
        ),
        (
            "git_text_partial",
            allocated_by(|| git_text_partial(path, &args).unwrap().0.len()).1,
        ),
        (
            "git_text_with_timeout",
            allocated_by(|| {
                git_text_with_timeout(path, &args, Duration::from_secs(60))
                    .unwrap()
                    .len()
            })
            .1,
        ),
    ];
    for (route, cost) in routes {
        eprintln!("{route}: {cost} bytes allocated against {baseline} for the bytes alone");
        assert!(
            cost <= baseline + slack,
            "{route} allocated {} bytes more than the same read returning bytes: a second copy",
            cost.saturating_sub(baseline)
        );
    }
}
