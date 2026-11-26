//! `store.vacuum`: the explicit, caller-triggered full rewrite of a profile.
//!
//! `store.compact` deletes rows, and the pages they held return to the
//! operating system only through an incremental vacuum, which SQLite runs only
//! on a database whose `auto_vacuum` is INCREMENTAL. Every profile created
//! before that setting existed has `auto_vacuum=NONE`, and SQLite cannot change
//! it except by rewriting the whole file, so such a profile keeps every freed
//! page forever. This method is that rewrite. It sets `auto_vacuum=INCREMENTAL`
//! first, so the one `VACUUM` both returns today's free pages and lets every
//! later `store.compact` return its own; there is no second full rewrite.
//!
//! It is never automatic and never part of a migration:
//!
//! * a host must send the request; no open, read, upgrade or compaction does;
//! * an older profile is refused (`schema_upgrade_required`) rather than
//!   upgraded first, so the rewrite never shares a request with a migration
//!   and its backup;
//! * it refuses, with a typed reason and before touching a page, when the disk
//!   cannot hold the rewrite (`insufficient_disk_space`) or when free space
//!   cannot be measured (`disk_space_unknown`): a check that could not run is
//!   not a check that passed.
//!
//! A `VACUUM` holds the write lock for its whole duration (minutes on a
//! multi-gigabyte profile). Run `store.compact` until it reports nothing
//! `truncated`, then this, with the profile's other writers quiet.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::compaction::checkpoint_wal;
use super::input::Input;
use super::{Error, MAX_INTEGER, Result};

/// Headroom required on top of the rewrite itself, unless the caller sets
/// `reserve_bytes`.
const DEFAULT_RESERVE: i64 = 1 << 30;
/// How long a free-space measurement may take before it counts as unknown.
const PROBE_WAIT: Duration = Duration::from_secs(10);

/// What one filesystem can hold.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Space {
    /// The mount point; two paths on one mount share one pool of free space.
    mount: String,
    available: u64,
}

/// One place the rewrite writes to, and what was measured there.
struct Target {
    label: &'static str,
    space: std::result::Result<Space, String>,
}

/// What the disk check concluded: the tightest filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Headroom {
    required: u64,
    available: u64,
}

pub(super) fn vacuum(input: &Input<'_>) -> Result<String> {
    input.fields(&["reserve_bytes"])?;
    let reserve = input.integer("reserve_bytes", Some(DEFAULT_RESERVE), MAX_INTEGER)?;
    let conn = input.conn;
    // A full VACUUM cannot run inside a transaction, and one that the caller
    // holds open would also hide the pages this reports.
    if !conn.is_autocommit() {
        return Err(Error::invalid(
            "store.vacuum rewrites the file and cannot run inside a transaction",
        ));
    }
    let path = main_file(conn)?;
    let wal = journal_is_wal(conn)?;
    if wal {
        // Fold the WAL into the file so the sizes below are the file's. A
        // reader that blocks this is not an error here: the VACUUM does not
        // need the file to be folded, and the closing checkpoint reports it.
        checkpoint_wal(conn)?;
    }

    let before = Measure::take(conn, &path)?;
    if before.auto_vacuum == 2 && before.free_pages == 0 {
        return report(conn, "not_needed", &before, &before, 0, 0, "unchanged");
    }

    // The rewrite builds a temporary copy (in SQLite's temp directory, which
    // may be another volume) and writes it back through the profile's journal
    // or WAL. The database size is the bound on each; filesystems that are one
    // mount are asked to hold both.
    let need = before.logical_bytes();
    let headroom = check_disk(
        &[
            Target {
                label: "the profile's directory",
                space: probe(path.parent().unwrap_or_else(|| Path::new("."))),
            },
            Target {
                label: "SQLite's temporary directory",
                space: probe(&sqlite_temp_dir()),
            },
        ],
        need,
        u64::try_from(reserve).map_err(|_| Error::invalid("reserve_bytes is out of range"))?,
    )?;

    // Set before the VACUUM: SQLite applies a changed auto_vacuum mode when the
    // file is rewritten, and only then.
    conn.pragma_update(None, "auto_vacuum", 2)?;
    conn.execute_batch("VACUUM").map_err(vacuum_error)?;

    let checkpoint = if wal {
        if checkpoint_wal(conn)?.cleared() {
            "cleared"
        } else {
            "blocked"
        }
    } else {
        "not_wal"
    };
    let after = Measure::take(conn, &path)?;
    report(
        conn,
        "vacuumed",
        &before,
        &after,
        headroom.required,
        headroom.available,
        checkpoint,
    )
}

/// The profile's size, as pages and as the file holding them.
struct Measure {
    page_size: i64,
    page_count: i64,
    free_pages: i64,
    auto_vacuum: i64,
    file_bytes: i64,
}

impl Measure {
    fn take(conn: &Connection, path: &Path) -> Result<Self> {
        let pragma = |name: &str| -> Result<i64> {
            Ok(conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))?)
        };
        let file_bytes = std::fs::metadata(path)
            .map_err(|error| Error::invalid(format!("reading the profile's size failed: {error}")))?
            .len();
        Ok(Self {
            page_size: pragma("page_size")?,
            page_count: pragma("page_count")?,
            free_pages: pragma("freelist_count")?,
            auto_vacuum: pragma("auto_vacuum")?,
            file_bytes: i64::try_from(file_bytes)
                .map_err(|_| Error::invalid("the profile's size is out of range"))?,
        })
    }

    /// The database as SQLite counts it: every page, free ones included.
    fn logical_bytes(&self) -> u64 {
        u64::try_from(self.page_size.saturating_mul(self.page_count)).unwrap_or(0)
    }
}

fn report(
    conn: &Connection,
    outcome: &str,
    before: &Measure,
    after: &Measure,
    required: u64,
    available: u64,
    checkpoint: &str,
) -> Result<String> {
    let bytes = |measure: &Measure| i64::try_from(measure.logical_bytes()).unwrap_or(i64::MAX);
    let clamp = |value: u64| i64::try_from(value).unwrap_or(i64::MAX);
    Ok(conn.query_row(
        "SELECT json_object('ok',json('true'),
            'item',json_object('id','vacuum','revision',1,'outcome',?1,
              'auto_vacuum_before',?2,'auto_vacuum_after',?3,
              'bytes_before',?4,'bytes_after',?5,
              'file_bytes_before',?6,'file_bytes_after',?7,
              'free_pages_before',?8,'free_pages_after',?9,
              'required_bytes',?10,'available_bytes',?11,'checkpoint',?12),
            'sequence',(SELECT coalesce(max(sequence),0) FROM work_events))",
        rusqlite::params![
            outcome,
            before.auto_vacuum,
            after.auto_vacuum,
            bytes(before),
            bytes(after),
            before.file_bytes,
            after.file_bytes,
            before.free_pages,
            after.free_pages,
            clamp(required),
            clamp(available),
            checkpoint,
        ],
        |row| row.get(0),
    )?)
}

fn main_file(conn: &Connection) -> Result<PathBuf> {
    let path: String = conn.query_row(
        "SELECT file FROM pragma_database_list WHERE name='main'",
        [],
        |row| row.get(0),
    )?;
    if path.is_empty() {
        return Err(Error::invalid(
            "an in-memory or temporary profile has no file to vacuum",
        ));
    }
    Ok(PathBuf::from(path))
}

fn journal_is_wal(conn: &Connection) -> Result<bool> {
    let journal: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    Ok(journal.eq_ignore_ascii_case("wal"))
}

/// SQLite's temporary directory, as far as the environment says: `SQLITE_TMPDIR`
/// when set, else the platform's (`TMPDIR`, or `/tmp`). SQLite's own search
/// also tries `/var/tmp` and `/usr/tmp`, which this does not model.
fn sqlite_temp_dir() -> PathBuf {
    std::env::var_os("SQLITE_TMPDIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// Whether every target can hold `need` bytes plus `reserve`; a mount that two
/// targets share must hold both copies.
fn check_disk(targets: &[Target], need: u64, reserve: u64) -> Result<Headroom> {
    let mut tightest: Option<Headroom> = None;
    let mut slack = i128::MAX;
    for target in targets {
        let space = target.space.as_ref().map_err(|reason| Error {
            code: "disk_space_unknown",
            message: format!(
                "free space for {} could not be measured, so the vacuum was not started: {reason}",
                target.label
            ),
        })?;
        let sharing = targets
            .iter()
            .filter(|other| matches!(&other.space, Ok(s) if s.mount == space.mount))
            .count() as u64;
        let required = need.saturating_mul(sharing).saturating_add(reserve);
        if space.available < required {
            return Err(Error {
                code: "insufficient_disk_space",
                message: format!(
                    "a vacuum needs about {required} bytes free on {} (the database is {need} bytes{}{}), \
                     but {} has {} bytes available; nothing was changed",
                    target.label,
                    if sharing > 1 {
                        ", and the temporary copy shares this volume"
                    } else {
                        ""
                    },
                    if reserve > 0 {
                        format!(", plus a {reserve}-byte reserve")
                    } else {
                        String::new()
                    },
                    space.mount,
                    space.available
                ),
            });
        }
        let margin = i128::from(space.available) - i128::from(required);
        if margin < slack {
            slack = margin;
            tightest = Some(Headroom {
                required,
                available: space.available,
            });
        }
    }
    tightest.ok_or_else(|| Error {
        code: "disk_space_unknown",
        message: "no location to measure free space for".into(),
    })
}

/// Free space on the filesystem holding `path`, from `df -Pk` (POSIX output).
/// The standard library has no such call, the workspace forbids `unsafe`, and
/// this crate takes no dependency for it. Anywhere `df` cannot be run the
/// answer is an error, which `check_disk` turns into a refusal.
fn probe(path: &Path) -> std::result::Result<Space, String> {
    let mut child = Command::new("/bin/df")
        .arg("-Pk")
        .arg(path)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("df could not be started: {error}"))?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < PROBE_WAIT => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("df did not answer within {PROBE_WAIT:?}"));
            }
            Err(error) => return Err(format!("waiting for df failed: {error}")),
        }
    };
    if !status.success() {
        return Err(format!("df failed for {}: {status}", path.display()));
    }
    let mut text = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        std::io::Read::read_to_string(&mut stdout, &mut text)
            .map_err(|error| format!("df output was not readable: {error}"))?;
    }
    parse_df(&text)
}

/// The `df -Pk` data line: `Filesystem 1024-blocks Used Available Capacity
/// Mounted-on`. The first and last names may hold spaces, so the line is read
/// from the capacity column (`NN%`), which they cannot contain.
fn parse_df(text: &str) -> std::result::Result<Space, String> {
    let line = text
        .lines()
        .nth(1)
        .ok_or_else(|| format!("df printed no data line: {text:?}"))?;
    let words: Vec<&str> = line.split_whitespace().collect();
    let capacity = words
        .iter()
        .rposition(|word| {
            word.ends_with('%')
                && word[..word.len() - 1].bytes().all(|b| b.is_ascii_digit())
                && word.len() > 1
        })
        .filter(|index| *index >= 3 && index + 1 < words.len())
        .ok_or_else(|| format!("df line has no capacity column: {line:?}"))?;
    let kib: u64 = words[capacity - 1]
        .parse()
        .map_err(|_| format!("df available column is not a number: {line:?}"))?;
    Ok(Space {
        mount: words[capacity + 1..].join(" "),
        available: kib.saturating_mul(1024),
    })
}

fn vacuum_error(error: rusqlite::Error) -> Error {
    use rusqlite::ErrorCode::{DatabaseBusy, DatabaseLocked, DiskFull};
    match &error {
        rusqlite::Error::SqliteFailure(failure, _)
            if matches!(failure.code, DatabaseBusy | DatabaseLocked) =>
        {
            Error {
                code: "vacuum_blocked",
                message: format!(
                    "another connection holds the profile, so the vacuum did not run and nothing was changed: {error}"
                ),
            }
        }
        rusqlite::Error::SqliteFailure(failure, _) if failure.code == DiskFull => Error {
            code: "insufficient_disk_space",
            message: format!(
                "the disk filled during the vacuum; SQLite rolled it back and the profile is as it was: {error}"
            ),
        },
        _ => error.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space(mount: &str, available: u64) -> Target {
        Target {
            label: "somewhere",
            space: Ok(Space {
                mount: mount.into(),
                available,
            }),
        }
    }

    #[test]
    fn df_output_is_read_from_the_capacity_column() {
        let plain = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                     /dev/disk3s5 971350180 764000000 176000000 82% /System/Volumes/Data\n";
        assert_eq!(
            parse_df(plain),
            Ok(Space {
                mount: "/System/Volumes/Data".into(),
                available: 176_000_000 * 1024
            })
        );
        // Spaces in the filesystem and in the mount point.
        let spaced = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                      my disk 1000 400 600 40% /Volumes/Back up\n";
        assert_eq!(
            parse_df(spaced),
            Ok(Space {
                mount: "/Volumes/Back up".into(),
                available: 600 * 1024
            })
        );
        for broken in [
            "",
            "Filesystem\n",
            "h\nonly three words\n",
            "h\na b c d e f\n",
        ] {
            assert!(parse_df(broken).is_err(), "{broken:?} parsed");
        }
    }

    #[test]
    fn a_disk_that_cannot_hold_the_rewrite_is_refused_with_its_numbers() {
        let error = check_disk(&[space("/a", 1_000)], 800, 300).unwrap_err();
        assert_eq!(error.code, "insufficient_disk_space");
        assert!(error.message.contains("1100"), "{}", error.message);
        assert!(error.message.contains("1000"), "{}", error.message);
        // Exactly enough passes.
        let ok = check_disk(&[space("/a", 1_100)], 800, 300).unwrap();
        assert_eq!(
            ok,
            Headroom {
                required: 1_100,
                available: 1_100
            }
        );
    }

    #[test]
    fn two_targets_on_one_mount_must_hold_both_copies() {
        // 1,000 would hold one 800-byte copy but not two.
        let error = check_disk(&[space("/a", 1_000), space("/a", 1_000)], 800, 0).unwrap_err();
        assert_eq!(error.code, "insufficient_disk_space");
        // On separate mounts each holds one.
        assert!(check_disk(&[space("/a", 1_000), space("/b", 1_000)], 800, 0).is_ok());
    }

    #[test]
    fn a_measurement_that_could_not_be_made_is_a_refusal_not_a_pass() {
        let unmeasured = Target {
            label: "the temp dir",
            space: Err("df could not be started".into()),
        };
        let error = check_disk(&[space("/a", u64::MAX), unmeasured], 1, 0).unwrap_err();
        assert_eq!(error.code, "disk_space_unknown");
        assert!(error.message.contains("the temp dir"), "{}", error.message);
        assert_eq!(
            check_disk(&[], 1, 0).unwrap_err().code,
            "disk_space_unknown"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_real_probe_reads_this_machines_free_space() {
        let space = probe(&std::env::temp_dir()).expect("df answers for the temp dir");
        assert!(space.available > 0);
        assert!(space.mount.starts_with('/'), "{space:?}");
        assert!(probe(Path::new("/definitely/not/a/path")).is_err());
    }
}
