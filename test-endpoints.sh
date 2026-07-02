#!/usr/bin/env bash
# Zügli UI-builder endpoint tests (M1 HW gate). See VERIFY.md.
#
# Usage:
#   ./test-endpoints.sh                    # full gate against zugli.local
#   ./test-endpoints.sh 192.168.1.237      # against an explicit host/IP
#   ./test-endpoints.sh 192.168.1.237 --focus   # one check: config|layout|focus|default|custom|save|clear
#   ./test-endpoints.sh --focus            # one check against zugli.local
#
# For the mode-switch checks to be visible on the panel, a stop must be selected on the device.
#
# NOTE: the config server serves ONE connection at a time and keeps each open ~5 s for HTTP
# keep-alive. We send "Connection: close" and pace requests so they don't collide.

set -u

HOST="zugli.local"
SEL=""
for a in "$@"; do
  case "$a" in
    --*) SEL="$a" ;;
    *)   HOST="$a" ;;
  esac
done
BASE="http://${HOST}"

# curl helper: show errors, cap time, force the server to close the connection immediately.
req() { curl -sS --max-time 8 -H "Connection: close" "$@"; local rc=$?; [ $rc -ne 0 ] && echo "  [curl error $rc — no response]"; return 0; }
pace() { sleep 0.5; }

hr() { printf '\n\033[1m== %s ==\033[0m\n' "$1"; }

CFG_DEFAULT='{"stripCity":false,"showLineBadges":true,"brightness":6,"autoBrightness":true,"offWhenDimmed":false,"reducedStart":1200,"reducedEnd":480,"uiMode":0}'
cfg_with_mode() { echo "${CFG_DEFAULT/\"uiMode\":0/\"uiMode\":$1}"; }

check_config() {
  hr "GET /config  (expect \"uiMode\":0, and NO \"focusView\")"
  req "${BASE}/config"; echo; pace
}

check_layout() {
  hr "GET /layout  (expect {\"v\":1,\"e\":[]} when none saved)"
  req "${BASE}/layout"; echo; pace
}

set_mode() { # $1 = uiMode, $2 = description
  hr "POST /config uiMode=$1  ($2)"
  req -X POST "${BASE}/config" -H 'Content-Type: application/json' -d "$(cfg_with_mode "$1")"; echo; pace
  printf '   -> readback: '; req "${BASE}/config"; echo; pace
}

save_layout() {
  hr "POST /layout  (save a tiny clock element, then read it back)"
  req -X POST "${BASE}/layout" -d '{"v":1,"e":[{"t":3,"x":2,"y":2,"s":1}]}'; echo; pace
  printf '   -> readback: '; req "${BASE}/layout"; echo; pace
  echo "   -> now press the board's EN/reset button (NOT BOOT), then run:"
  echo "        ./test-endpoints.sh ${HOST} --layout    (should still return the clock element)"
}

clear_layout() {
  hr "POST /layout empty  (clear the saved layout)"
  req -X POST "${BASE}/layout" -d '{"v":1,"e":[]}'; echo; pace
  printf '   -> readback: '; req "${BASE}/layout"; echo; pace
}

echo "host: ${BASE}"
case "${SEL}" in
  --config)  check_config ;;
  --layout)  check_layout ;;
  --focus)   set_mode 1 "panel should switch to the Focus view" ;;
  --default) set_mode 0 "panel back to the default board" ;;
  --custom)  set_mode 2 "Custom; empty layout falls back to the default board" ;;
  --save)    save_layout ;;
  --clear)   clear_layout ;;
  *)
    check_config
    check_layout
    set_mode 1 "panel should switch to the Focus view"
    set_mode 0 "panel back to the default board"
    set_mode 2 "Custom; empty layout falls back to the default board"
    set_mode 0 "panel back to the default board"
    save_layout
    ;;
esac
