#!/usr/bin/env python3
"""Adds env-gated (L4_MUT=<name>) production mutations to the L4 lane worktree.
Inert unless L4_MUT is set. Revert with `git checkout -- <files>` (see run_matrix.sh)."""
import sys, pathlib
W = pathlib.Path(sys.argv[1]) / "src-tauri" / "src"

def sub(path, old, new):
    p = W / path
    s = p.read_text()
    assert s.count(old) == 1, (path, old, s.count(old))
    p.write_text(s.replace(old, new))

M = 'std::env::var("L4_MUT").as_deref() == Ok("{}")'

# hooks: parse itself slow (CPU, on the measured thread)
sub("hooks/notify_input.rs",
 "pub(super) fn kept_fields(bytes: &[u8]) -> Result<HookInput, String> {\n",
 "pub(super) fn kept_fields(bytes: &[u8]) -> Result<HookInput, String> {\n"
 "    if " + M.format("hooks_parse_slow") + " {\n"
 "        let mut acc = 0u64;\n"
 "        for _ in 0..150 { for b in bytes { acc = acc.wrapping_add(u64::from(*b)); } }\n"
 "        std::hint::black_box(acc);\n    }\n")
# hooks: the reader around the parse slow (1.2 s of CPU spin, on the worker thread)
sub("hooks/notify_input.rs",
 "        kept_fields(&bytes)\n    })\n",
 "        if " + M.format("hooks_reader_slow") + " {\n"
 "            let t = std::time::Instant::now();\n"
 "            while t.elapsed() < Duration::from_millis(1200) { std::hint::black_box(0u8); }\n        }\n"
 "        kept_fields(&bytes)\n    })\n")
# docs: a query waits for the build lock
sub("docs/mod.rs",
 "    let slot = vault_slot(&repo);\n    if !force {\n        if let Some(value) = slot.current() {",
 "    let slot = vault_slot(&repo);\n"
 "    if !force && " + M.format("docs_block") + " {\n"
 "        drop(slot.build.lock().unwrap_or_else(std::sync::PoisonError::into_inner));\n    }\n"
 "    if !force {\n        if let Some(value) = slot.current() {")
# alerts: a turn_finished is acknowledged and dropped
sub("alerts/intake.rs",
 "        if notice.origin == Origin::Hook {\n            if let Some(refused) = self.screen(&notice, stamp, now) {",
 "        if notice.event == Some(\"turn_finished\") && " + M.format("order_drop") + " {\n            return Offer::Accepted;\n        }\n"
 "        if notice.origin == Origin::Hook {\n            if let Some(refused) = self.screen(&notice, stamp, now) {")
# alerts: every key draws on the shared outside bucket
sub("alerts/intake.rs",
 'let outside = notice.key.starts_with("hook-");',
 'let outside = notice.key.starts_with("hook-") || ' + M.format("outside_all") + ';')
# alerts: the drain does nothing / one token short
sub("alerts/intake.rs",
 "        let mut limits = self\n            .limits\n            .lock()\n            .unwrap_or_else(std::sync::PoisonError::into_inner);\n        let now = std::time::Instant::now();\n        (0..super::limits::OUTSIDE_BURST as usize + 1)",
 "        if " + M.format("drain_noop") + " {\n            return true;\n        }\n"
 "        let mut limits = self\n            .limits\n            .lock()\n            .unwrap_or_else(std::sync::PoisonError::into_inner);\n        let now = std::time::Instant::now();\n        (0..super::limits::OUTSIDE_BURST as usize + 1)")
# git_cli: band skipped / band faked (counter says entered, kernel never asked)
sub("engine/git_cli.rs",
 "        note_band_outcome(label, crate::procguard::enter_background_band(&child));\n",
 "        match std::env::var(\"L4_MUT\").as_deref() {\n"
 "            Ok(\"band_skip\") => {}\n"
 "            Ok(\"band_fake\") => note_band_outcome(label, Ok(crate::procguard::BandOutcome::Entered)),\n"
 "            _ => note_band_outcome(label, crate::procguard::enter_background_band(&child)),\n        }\n")
print("applied")
