#!/usr/bin/env bash

# Interactive RGV native-interaction stress worksheet for macOS.
#
# Run from the repository root. Select one section at a time in Neovim and send
# it to the same tmux shell with vim-slime. Do not send the whole file at once:
# native sections take over the RGV window and manual recording pauses for use.
#
# Native sections take control of the visible RGV window. Guarded mouse/scroll
# delivery briefly moves the system pointer and restores it when the run ends.
# Grant Accessibility and Input Monitoring permission to the terminal that runs
# the Swift driver.

#####################
# 1. BUILD — CONTROLLER PANE
#####################

cargo build --release -p rgv-desktop -p rgv-interaction-stress
swift build -c release --package-path tools/interaction-stress/macos-driver

#####################
# 2. OFFLINE SCENARIO CHECK — CONTROLLER PANE
#####################

target/release/rgv-interaction-stress validate \
  validation/interaction-stress/scenarios/round-trip-smoke.json

target/release/rgv-interaction-stress mutate \
  validation/interaction-stress/scenarios/round-trip-smoke.json \
  --output /tmp/rgv-round-trip-mutated.json \
  --seed 41 \
  --intensity 6

target/release/rgv-interaction-stress validate \
  /tmp/rgv-round-trip-mutated.json

#####################
# 3. CREATE THE LOCAL TEST SECRET ONCE — CONTROLLER PANE
#####################

# These lines replace any existing interaction-stress secret. Restart RGV after
# replacing it so the application and attach client read the same value.
umask 077
openssl rand -hex 32 > /private/tmp/rgv-interaction-stress.secret

#####################
# 4. LAUNCH ONE VISIBLE RGV WINDOW — CONTROLLER PANE
#####################

# RGV runs as a background job in this shell. Its PID is retained for section
# 12, and stdout/stderr remain available in the log instead of occupying Bash.
target/release/rgv-desktop \
  --control-port 60151 \
  --control-secret-file /private/tmp/rgv-interaction-stress.secret \
  --control-root "$PWD" \
  < /dev/null > /tmp/rgv-interaction-stress-app.log 2>&1 &

rgv_demo_app_pid=$!
sleep 2
ps -p "$rgv_demo_app_pid" -o pid=,state=,command=
tail -n 20 /tmp/rgv-interaction-stress-app.log

#####################
# 5. PROBE AND COMPILE WITHOUT NATIVE INPUT — CONTROLLER PANE
#####################

target/release/rgv-interaction-stress probe \
  --attach validation/interaction-stress/attach.smoke.example.json

target/release/rgv-interaction-stress replay \
  validation/interaction-stress/scenarios/round-trip-smoke.json \
  --attach validation/interaction-stress/attach.smoke.example.json \
  --dry-run

#####################
# 6. RUN THE NATIVE SMOKE SCENARIO — CONTROLLER PANE
#####################

# This focuses RGV, pans and zooms the canvas, cycles focus, and resizes the
# window. Keep your hands off the mouse and keyboard until it finishes.
target/release/rgv-interaction-stress replay \
  validation/interaction-stress/scenarios/round-trip-smoke.json \
  --attach validation/interaction-stress/attach.smoke.example.json

#####################
# 7. GENERATE A NEW STATE-AWARE SCENARIO — CONTROLLER PANE
#####################

# Generation-only reads live workspace state but does not post native input.
target/release/rgv-interaction-stress generate \
  --attach validation/interaction-stress/attach.smoke.example.json \
  --seed 20260904 \
  --actions 12 \
  --profile smoke \
  --maximum-no-wait-actions 2 \
  --output /tmp/rgv-generated-smoke.json \
  --generate-only

target/release/rgv-interaction-stress validate \
  /tmp/rgv-generated-smoke.json

target/release/rgv-interaction-stress replay \
  /tmp/rgv-generated-smoke.json \
  --attach validation/interaction-stress/attach.smoke.example.json \
  --dry-run

#####################
# 8. REPLAY THE GENERATED SCENARIO NATIVELY — CONTROLLER PANE
#####################

# Keep your hands off the mouse and keyboard until this finishes.
target/release/rgv-interaction-stress replay \
  /tmp/rgv-generated-smoke.json \
  --attach validation/interaction-stress/attach.smoke.example.json

#####################
# 9. RECORD 30 SECONDS OF MANUAL USE — CONTROLLER PANE
#####################

# Send this entire block, switch to RGV, and interact normally for 30 seconds.
# Text-producing keys are intentionally not recorded by this example.
rgv_demo_stamp="$(date +%Y%m%d-%H%M%S)"
rgv_recording_path="validation/interaction-stress/recordings/manual-${rgv_demo_stamp}.jsonl"
rgv_scenario_path="/tmp/rgv-manual-${rgv_demo_stamp}.json"

target/release/rgv-interaction-stress record \
  --attach validation/interaction-stress/attach.smoke.example.json \
  --output "$rgv_recording_path" \
  --duration-seconds 30

target/release/rgv-interaction-stress convert \
  "$rgv_recording_path" \
  --output "$rgv_scenario_path"

target/release/rgv-interaction-stress validate \
  "$rgv_scenario_path"

#####################
# 10. REPLAY THE MANUAL RECORDING — CONTROLLER PANE
#####################

# This section expects the variables from section 9 to remain in this shell.
# Keep your hands off the mouse and keyboard until it finishes.
target/release/rgv-interaction-stress replay \
  "$rgv_scenario_path" \
  --attach validation/interaction-stress/attach.smoke.example.json

#####################
# 11. LOCATE RUN ARTIFACTS — CONTROLLER PANE
#####################

ls -lt validation/interaction-stress/runs

#####################
# 12. STOP THE BACKGROUND RGV PROCESS — CONTROLLER PANE
#####################

# Use this when testing is finished. It targets only the PID captured in
# section 4. Closing the RGV window normally is also fine.
kill -TERM "$rgv_demo_app_pid"
wait "$rgv_demo_app_pid"
