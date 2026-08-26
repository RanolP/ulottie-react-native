#!/bin/bash
# Parity sweep driver: drives the compare app fixture-by-fixture, screenshots
# each pinned frame, and crops the three 300x300dp player regions (@3x ->
# 900px). Coordinates come from the app's PARITY_LAYOUT log: ulottie (51,188),
# ulottie-skia (51,488), scale 3 (1206x2622 screenshot over a 402x874 logical
# screen). The lottie box (51,788) hangs past the screen bottom, so it is
# captured from a second, bottom-scrolled screenshot where the players
# ScrollView's last box bottom-aligns with the screen: crop y = 2622 - 900.
#
# Element location goes through `agent-device snapshot --json` (testID lands in
# `identifier`) + `press x y`: `find id=...` proved unreliable for chips inside
# the horizontal picker, coordinates are not.
set -u
UDID=C74B3DD9-DE8D-4DEA-BC93-7E29B6BED484
ART=/Users/ranolp/syncthing/Projects/ulottie-react-native/examples/compare/.artifacts
OUT=$ART/parity
mkdir -p "$OUT"
FIXTURES=(${FIXTURES_OVERRIDE:-boucing_ball rectangle ellipse fill trim_path android_wave precomp_star_circle gradient_radial lottie_logo_1 mask_subtract matte_alpha stroke_under_fill blend_multiply gradient_animated matte_luma_inv fx_effects image_embedded})
# Fixtures the svg target refuses: the ulottie box shows a placeholder, so no
# `_u` crop is taken (parity_table.mjs skips the svg column for these too).
SKIA_ONLY=" blend_multiply gradient_animated matte_luma_inv fx_effects image_embedded "
PCTS=(0 25 50 75 100)

state() { agent-device get text "id=parity-state" 2>/dev/null | tail -1; }

# Print "cx cy" of the identified element if fully on-screen, else nothing.
locate() {
  agent-device snapshot --json 2>/dev/null | jq -r --arg id "$1" '
    .data.nodes[] | select(.identifier == $id) | .rect
    | select(.x >= 0 and (.x + .width) <= 402)
    | "\(.x + .width / 2 | floor) \(.y + .height / 2 | floor)"' | head -1
}

press_id() {
  local xy
  xy=$(locate "$1")
  [[ -n "$xy" ]] || return 1
  agent-device press $xy >/dev/null 2>&1
}

select_fixture() {
  local name=$1
  # Reset picker to its left edge, then pan left until the chip is pressable.
  for _ in 1 2 3 4 5; do
    agent-device gesture pan 120 117 260 0 >/dev/null 2>&1
    sleep 0.3
  done
  for _ in 0 1 2 3 4 5 6 7 8; do
    if press_id "fixture-$name"; then
      [[ "$(state)" == "$name @"* ]] && return 0
    fi
    agent-device gesture pan 300 117 -120 0 >/dev/null 2>&1
    sleep 0.3
  done
  echo "FAILED to select $name (state: $(state))" >&2
  return 1
}

select_frame() {
  local name=$1 pct=$2
  for _ in 0 1 2; do
    press_id "frame-$pct"
    [[ "$(state)" == "$name @ $pct%" ]] && return 0
  done
  echo "FAILED to select frame $pct of $name (state: $(state))" >&2
  return 1
}

# Scroll the players ScrollView hard to one end. $1: -500 scrolls toward the
# bottom (content up), 500 toward the top; three pans overshoot either way.
scroll_players() {
  for _ in 1 2 3; do
    agent-device gesture pan 200 600 0 "$1" >/dev/null 2>&1
    sleep 0.3
  done
  sleep 0.5 # let the overscroll bounce settle before the screenshot
}

for name in "${FIXTURES[@]}"; do
  select_fixture "$name" || continue
  for pct in "${PCTS[@]}"; do
    select_frame "$name" "$pct" || continue
    sleep 0.7
    shot=$OUT/_full.png
    scroll_players 500
    xcrun simctl io "$UDID" screenshot "$shot" >/dev/null 2>&1
    if [[ "$SKIA_ONLY" != *" $name "* ]]; then
      sips -c 900 900 --cropOffset 564 153 "$shot" --out "$OUT/${name}_${pct}_u.png" >/dev/null
    fi
    sips -c 900 900 --cropOffset 1464 153 "$shot" --out "$OUT/${name}_${pct}_s.png" >/dev/null
    scroll_players -500
    xcrun simctl io "$UDID" screenshot "$shot" >/dev/null 2>&1
    sips -c 900 900 --cropOffset 1722 153 "$shot" --out "$OUT/${name}_${pct}_l.png" >/dev/null
    echo "captured ${name}_${pct}"
  done
done
rm -f "$OUT/_full.png"
echo DONE
