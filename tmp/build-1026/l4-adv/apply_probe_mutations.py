#!/usr/bin/env python3
"""Puts three of the lane's repaired probes back to the session-less form the base had, in
alerts/bridge.rs `mod tests`. With the lane's `own_listener()` drain intact, each of these is a
regression the drain is documented to catch every time ("a probe that names no session is refused as
`rate_limited` here every time"). A test that still PASSES is a BREAK of that claim."""
import sys, pathlib

p = pathlib.Path(sys.argv[1]) / "src-tauri" / "src" / "alerts" / "bridge.rs"
s = p.read_text()
LIT = 'br#"{"v":1,"event":"error","agent":"codex"}"#'

def sub(old, new):
    global s
    assert s.count(old) == 1, (old, s.count(old))
    s = s.replace(old, new)

sub('let after = report_as("error", "codex", &unique("after-flood"));',
    f'let after = {LIT}.to_vec();')
sub('send(&dir.socket(), &report_as("error", "codex", &unique("idle"))).trim(),',
    f'send(&dir.socket(), {LIT}).trim(),')
sub('''                send(
                    &dir.socket(),
                    &report_as("error", "codex", &unique("restart"))
                )
                .trim(),''',
    f'''                send(&dir.socket(), {LIT}).trim(),''')
p.write_text(s)
print("probe mutations applied")
