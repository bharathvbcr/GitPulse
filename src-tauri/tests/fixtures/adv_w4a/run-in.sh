#!/bin/bash
# Inside the container (see run.sh). Every catchable signal is ignored here and
# inherited by everything below, so a real pkill/killall that selects too much
# is harmless, and the command runs under a GitPulse-named copy of bash so a
# pkill that reaches its caller's ancestors reaches a decoy.
trap '' TERM HUP INT QUIT USR1 USR2 ALRM
cp /bin/bash /tmp/gitpulse-anc
/tmp/gitpulse-anc -c 'trap "" TERM HUP INT QUIT USR1 USR2 ALRM; cd /work/src-tauri && "$@"; echo "exit=$?"; true' x "$@"
