#!/bin/bash
# Build an fpga/projects/<name> overlay against the ADI hdl checkout.
# Build output goes to $FPGA_BUILD (default ~/.cache/amateur-radio-fpga/build/<name>).
# Build only. This never programs or flashes any device.
set -euo pipefail
name="${1:?usage: build_project.sh <project-name>}"
here="$(cd "$(dirname "$0")/.." && pwd)"
export ADI_HDL_DIR="${ADI_HDL_DIR:-$HOME/.cache/amateur-radio-fpga/hdl}"
export ADI_IGNORE_VERSION_CHECK=1          # hdl_2026_r1 targets Vivado 2025.1; we use 2026.1
out="${FPGA_BUILD:-$HOME/.cache/amateur-radio-fpga/build}/$name"
mkdir -p "$out"
rsync -a --delete --exclude '*.runs' --exclude '*.cache' --exclude '*.gen' --exclude '*.srcs' \
  --exclude '*.hw' --exclude '*.ip_user_files' --exclude '*.sdk' --exclude '*.xpr' \
  --exclude '*.log' --exclude '*.jou' --exclude 'reports' "$here/projects/$name/" "$out/"
for f in "$here"/rtl/*.v "$here"/rtl/*.sv; do [ -e "$f" ] && cp "$f" "$out/"; done
cd "$out"
rm -rf ./*.runs ./*.cache ./*.gen ./*.srcs ./*.hw ./*.ip_user_files ./*.sdk ./*.xpr .Xil
vivado -mode batch -nojournal -log vivado.log -source system_project.tcl
vivado -mode batch -nojournal -log reports.log -source "$here/scripts/reports.tcl" -tclargs "$name"
