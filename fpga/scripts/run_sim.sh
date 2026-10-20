#!/bin/bash
# Generate vectors for each scenario from the bit-accurate model and run tb_chan.
#   fpga/scripts/run_sim.sh [scenario ...]   (default: all scenarios)
set -uo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
repo="$(cd "$here/.." && pwd)"
cache="${FPGA_SIM:-$HOME/.cache/amateur-radio-fpga/sim}"
adi="${ADI_HDL_DIR:-$HOME/.cache/amateur-radio-fpga/hdl}"
# Vivado records absolute source paths. A project cached by another worktree can
# silently simulate its old compiled design after those paths disappear.
project="$cache/proj-$(printf '%s' "$here" | sha256sum | cut -c1-12)"
scenarios=("$@")
[ ${#scenarios[@]} -eq 0 ] && scenarios=(mixed fullscale random backpressure overflow reset snapshot autofwd)
mkdir -p "$cache"
if [ ! -f "$project/chan_sim.xpr" ]; then
  vivado -mode batch -nojournal -log "$cache/setup.log" -source "$here/scripts/sim_setup.tcl" \
    -tclargs "$here" "$project" "$adi" > /dev/null
fi
status=0
for s in "${scenarios[@]}"; do
  (cd "$repo" && PYTHONPATH=. uv run python -m fpga.model.golden vectors "$s" "$cache/vec_$s") > "$cache/vec_$s.json" || { echo "$s: VECTOR GENERATION FAILED"; status=1; continue; }
  rm -f "$cache/sim_$s.log" "$project"/chan_sim.sim/sim_1/behav/xsim/simulate.log
  vivado -mode batch -nojournal -log "$cache/sim_$s.log" -source "$here/scripts/sim_run.tcl" \
    -tclargs "$project" "$cache/vec_$s" > /dev/null 2>&1 || echo "$s: vivado exited non-zero"
  # only this scenario's own log, and only after its own banner
  result=$(sed -n "/tb_chan: .*vec_$s /,\$p" "$cache/sim_$s.log" | grep -o "TB_CHAN \(PASS\|FAIL.*\)" | tail -1)
  echo "$s: ${result:-NO RESULT (see $cache/sim_$s.log)}"
  [ "$result" = "TB_CHAN PASS" ] || status=1
done
exit $status
