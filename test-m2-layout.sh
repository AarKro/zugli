#!/usr/bin/env bash
# Zügli UI-builder M2 HW gate — custom-layout renderer. See VERIFY.md (M2).
#
# Switches the panel to Custom mode (uiMode=2) and POSTs one layout that exercises EVERY element
# type, both fonts, all three integer scales, a custom colour, and a missing departure slot — so a
# single glance at the panel checks the whole renderer.
#
# Usage:
#   ./test-m2-layout.sh                     # full gate against zugli.local
#   ./test-m2-layout.sh 192.168.1.237       # against an explicit host/IP
#   ./test-m2-layout.sh 192.168.1.237 --post    # one step: mode|post|read|default|clear
#   ./test-m2-layout.sh --clear             # clear the layout + back to the default board
#
# IMPORTANT: the live-data elements (departure badge/direction/time, station name, clock, date)
# only show real content when a stop is SELECTED on the device and has upcoming departures. With
# no stop selected, Custom still draws the static elements (text, divider, icons) and the empty
# departure slots simply draw nothing.
#
# NOTE: the config server serves ONE connection at a time and holds each ~5 s for keep-alive; we
# send "Connection: close" and pace requests so they don't collide.

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

# The test layout (64×64, y = baseline-top). 15 elements, compact keys; omitted fields take their
# schema defaults (x/y/w/s/c/a/di/fk/f/g → 0, k/th → 1). Coverage map:
#   t=0 Text     : "S1" (5x7, k1)  ·  "3" (5x7, k3, custom green col=0x50D888)  ·  "M2" (6x10, k2)
#   t=5 Divider  : full width, thickness 2, copper
#   t=1 Departure: slot0 badge+direction+time · slot1 time · slot2 time (usually MISSING → blank)
#   t=3 Clock    : 6x10, f=1  → "H:MM"
#   t=2 Station  : 5x7, marquee clipped to w=42
#   t=4 Date     : 5x7, f=0  → "DD.MM."
#   t=6 Icon     : tram-front (g0, k1) · Z-blind (g1, k1) · arrow (g2, k1)
read -r -d '' LAYOUT <<'JSON'
{"v":1,"e":[
{"t":0,"x":1,"y":0,"v":"S1"},
{"t":0,"x":15,"y":0,"k":3,"col":5296264,"v":"3"},
{"t":0,"x":30,"y":0,"s":1,"k":2,"c":1,"v":"M2"},
{"t":5,"x":0,"y":22,"th":2,"c":1},
{"t":1,"x":1,"y":26,"s":1,"di":0,"fk":0},
{"t":1,"x":16,"y":29,"w":30,"di":0,"fk":1},
{"t":1,"x":50,"y":29,"c":1,"di":0,"fk":2},
{"t":1,"x":50,"y":38,"c":1,"di":1,"fk":2},
{"t":1,"x":50,"y":47,"c":2,"di":2,"fk":2},
{"t":3,"x":1,"y":38,"s":1,"f":1},
{"t":2,"x":1,"y":49,"w":42,"c":1},
{"t":4,"x":1,"y":58,"c":2},
{"t":6,"x":48,"y":49,"g":0},
{"t":6,"x":46,"y":58,"g":1,"c":1},
{"t":6,"x":54,"y":58,"g":2,"c":2}
]}
JSON
# Collapse to one line for the POST body.
LAYOUT_ONELINE="$(printf '%s' "$LAYOUT" | tr -d '\n')"

set_custom() {
  hr "POST /config uiMode=2  (switch the panel to Custom mode)"
  req -X POST "${BASE}/config" -H 'Content-Type: application/json' -d "$(cfg_with_mode 2)"; echo; pace
  printf '   -> readback: '; req "${BASE}/config"; echo; pace
}

post_layout() {
  hr "POST /layout  (the all-element test layout — ${#LAYOUT_ONELINE} bytes)"
  req -X POST "${BASE}/layout" -H 'Content-Type: application/json' -d "$LAYOUT_ONELINE"; echo; pace
  cat <<'EOF'
   -> Now eyeball the PANEL (Custom mode). Expect, top to bottom:
        • row 0 : "S1" small · "3" TALL green (3× scale) · "M2" medium (2× scale)
        • a thick full-width copper divider
        • departure slot 0 : line badge · scrolling destination · minutes (copper)
        • departure slot 1 : minutes only (copper) — proves the 2nd live slot resolves
        • departure slot 2 : minutes only — BLANK unless 3 departures are live (missing slot = nothing)
        • clock "H:MM" (medium) on the left
        • station name (small, scrolls if long) clipped mid-width
        • date "DD.MM." (small) bottom-left
        • three icons bottom-right : tram-front · Z-blind · arrow
   -> Then press the board's EN/reset button (NOT BOOT) and run:
        ./test-m2-layout.sh HOST --read     (layout should persist across reboot)
EOF
}

read_layout() {
  hr "GET /layout  (read back the persisted layout)"
  req "${BASE}/layout"; echo; pace
}

revert_default() {
  hr "POST /config uiMode=0  (back to the built-in board)"
  req -X POST "${BASE}/config" -H 'Content-Type: application/json' -d "$(cfg_with_mode 0)"; echo; pace
}

clear_layout() {
  hr "POST /layout empty  (clear the saved layout — Custom now falls back to the default board)"
  req -X POST "${BASE}/layout" -H 'Content-Type: application/json' -d '{"v":1,"e":[]}'; echo; pace
  printf '   -> readback: '; req "${BASE}/layout"; echo; pace
}

echo "host: ${BASE}"
case "${SEL}" in
  --mode)    set_custom ;;
  --post)    post_layout ;;
  --read)    read_layout ;;
  --default) revert_default ;;
  --clear)   clear_layout; revert_default ;;
  *)
    set_custom
    post_layout
    read_layout
    ;;
esac
