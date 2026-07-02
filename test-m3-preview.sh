#!/usr/bin/env bash
# Zügli UI-builder M3 HW gate — live-preview endpoints + auto-revert watchdog. See VERIFY.md (M3).
#
# Verifies POST /preview shows on the panel REGARDLESS of uiMode, writes NO flash, and that both
# POST /preview/end and the ~15 s timeout revert to the device's persisted mode + layout.
#
# Usage:
#   ./test-m3-preview.sh                    # guided full sequence against zugli.local
#   ./test-m3-preview.sh 192.168.1.237      # explicit host/IP
#   ./test-m3-preview.sh HOST --seed        # persist the "clock" baseline (Custom mode) to compare against
#   ./test-m3-preview.sh HOST --preview     # one /preview push (watch panel switch, reverts in ~15 s)
#   ./test-m3-preview.sh HOST --keepalive   # push /preview every 5 s (simulates the editor); Ctrl-C to stop
#   ./test-m3-preview.sh HOST --end         # POST /preview/end (immediate revert)
#   ./test-m3-preview.sh HOST --read        # GET /layout (proves preview never persisted)
#
# For anything to be visible on the panel a stop must be SELECTED on the device (the preview only
# overrides the Departures screen). With no stop selected the panel is on the idle/address screen
# and the preview won't show — pick a stop first.
#
# NOTE: single-connection server, ~5 s keepalive; we send "Connection: close" and pace requests.

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

req() { curl -sS --max-time 8 -H "Connection: close" "$@"; local rc=$?; [ $rc -ne 0 ] && echo "  [curl error $rc — no response]"; return 0; }
pace() { sleep 0.5; }
hr() { printf '\n\033[1m== %s ==\033[0m\n' "$1"; }

CFG_DEFAULT='{"stripCity":false,"showLineBadges":true,"brightness":6,"autoBrightness":true,"offWhenDimmed":false,"reducedStart":1200,"reducedEnd":480,"uiMode":0}'
cfg_with_mode() { echo "${CFG_DEFAULT/\"uiMode\":0/\"uiMode\":$1}"; }

# Persisted baseline: a big clock at 2× scale. Distinct from the preview below so a revert/reboot is
# visually obvious (clock ≠ "PRE").
SAVED='{"v":1,"e":[{"t":3,"x":2,"y":24,"s":1,"k":2}]}'
# Transient preview: an unmistakable "PRE" banner (never touches flash).
PREVIEW='{"v":1,"e":[{"t":5,"x":0,"y":10,"th":2,"c":1},{"t":0,"x":5,"y":18,"s":1,"k":3,"v":"PRE"},{"t":6,"x":2,"y":52,"g":2,"k":2,"c":2}]}'

set_mode() { req -X POST "${BASE}/config" -H 'Content-Type: application/json' -d "$(cfg_with_mode "$1")" >/dev/null; }

seed_saved() {
  hr "SEED  (persist the clock layout + set Custom mode — this is the baseline to revert to)"
  req -X POST "${BASE}/layout" -H 'Content-Type: application/json' -d "$SAVED"; echo; pace
  set_mode 2; echo "   -> uiMode=2 (Custom). Panel should now show the big CLOCK."; pace
  printf '   -> saved /layout: '; req "${BASE}/layout"; echo; pace
}

push_preview() {
  hr "POST /preview  (transient 'PRE' banner — should appear on the panel WITHIN a second)"
  req -X POST "${BASE}/preview" -H 'Content-Type: application/json' -d "$PREVIEW"; echo; pace
  echo "   -> Watch the PANEL: it should switch from the clock to a 'PRE' banner + arrow,"
  echo "      then (with no further pushes) auto-revert to the clock after ~15 s."
}

keepalive() {
  hr "KEEPALIVE  (POST /preview every 5 s — simulates the editor being open; Ctrl-C to stop)"
  echo "   While this runs the panel stays on 'PRE'. Stop it (Ctrl-C) and the panel reverts to the"
  echo "   persisted layout (clock) within ~15 s — the abandoned-session watchdog."
  local n=0
  while true; do
    n=$((n+1))
    printf '   push #%d ... ' "$n"; req -X POST "${BASE}/preview" -H 'Content-Type: application/json' -d "$PREVIEW"; echo
    sleep 5
  done
}

end_preview() {
  hr "POST /preview/end  (immediate revert to the persisted mode + layout)"
  req -X POST "${BASE}/preview/end" -H 'Content-Type: application/json'; echo; pace
  echo "   -> Panel should revert to the CLOCK (persisted Custom layout) right away."
}

read_saved() {
  hr "GET /layout  (must still be the CLOCK — preview never persisted)"
  req "${BASE}/layout"; echo; pace
  echo "   -> Expect the t:3 clock element, NOT the 'PRE' text. If you rebooted after a /preview"
  echo "      push and this still shows the clock, the no-flash-write gate PASSES."
}

echo "host: ${BASE}"
case "${SEL}" in
  --seed)      seed_saved ;;
  --preview)   push_preview ;;
  --keepalive) keepalive ;;
  --end)       end_preview ;;
  --read)      read_saved ;;
  *)
    seed_saved
    push_preview
    cat <<EOF

   Guided checks (run these next, individually):
     1) NO-FLASH  : after the 'PRE' banner is showing, press EN/reset, then:
                       ./test-m3-preview.sh ${HOST} --read     (should return the CLOCK, not 'PRE')
     2) IMMEDIATE : ./test-m3-preview.sh ${HOST} --preview  then  ./test-m3-preview.sh ${HOST} --end
                       (panel reverts to the clock at once)
     3) TIMEOUT   : ./test-m3-preview.sh ${HOST} --preview  then wait ~15 s without pushing
                       (panel auto-reverts to the clock)
     4) KEEPALIVE : ./test-m3-preview.sh ${HOST} --keepalive , watch it hold 'PRE', Ctrl-C,
                       then the panel reverts within ~15 s
     5) MODE-INDEP: ./test-m3-preview.sh ${HOST} --seed is Custom; also try setting uiMode=0 via
                       test-endpoints.sh --default, then --preview here — 'PRE' still shows.
EOF
    ;;
esac
