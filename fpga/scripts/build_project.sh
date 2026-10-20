#!/bin/bash
# Build an fpga/projects/<name> overlay against the ADI hdl checkout.
# Build output goes to $FPGA_BUILD (default ~/.cache/amateur-radio-fpga/build/<name>).
# Build only. This never programs or flashes any device.
set -euo pipefail
usage() {
  echo "usage: build_project.sh [--clean|--incremental] [--dry-run] <project-name>" >&2
  exit 2
}

mode=clean
mode_set=0
dry_run=0
name=""
while (($#)); do
  case "$1" in
    --clean|--incremental)
      ((mode_set == 0)) || usage
      mode="${1#--}"
      mode_set=1
      ;;
    --dry-run)
      dry_run=1
      ;;
    --help|-h)
      echo "usage: build_project.sh [--clean|--incremental] [--dry-run] <project-name>"
      exit 0
      ;;
    -* )
      usage
      ;;
    *)
      [[ -z "$name" ]] || usage
      name="$1"
      ;;
  esac
  shift
done
[[ -n "$name" ]] || usage
here="$(cd "$(dirname "$0")/.." && pwd)"
export ADI_HDL_DIR="${ADI_HDL_DIR:-$HOME/.cache/amateur-radio-fpga/hdl}"
export FPGA_DIR="$here"
export ADI_IGNORE_VERSION_CHECK=1          # hdl_2026_r1 targets Vivado 2025.1; we use 2026.1
build_root="${FPGA_BUILD:-$HOME/.cache/amateur-radio-fpga/build}"
[[ "$build_root" == /* ]] || build_root="$PWD/$build_root"
out="$build_root/$name"
project_dir="$here/projects/$name"
[[ -f "$project_dir/system_project.tcl" ]] || { echo "unknown FPGA project: $name" >&2; exit 2; }
part="$(sed -n 's/.*adi_project_create [^\"]*\"\([^\"]*\)\".*/\1/p' "$project_dir/system_project.tcl" | head -1)"

reference_key() {
  local adi_revision adi_dirty project_hash
  adi_revision="$(git -C "$ADI_HDL_DIR" rev-parse HEAD)"
  adi_dirty="$(git -C "$ADI_HDL_DIR" status --porcelain --untracked-files=all)"
  project_hash="$(sha256sum "$project_dir/system_project.tcl" | awk '{ print $1 }')"
  [[ -z "$adi_dirty" ]] || return 1
  printf '%s|%s|%s|%s|%s' "$name" "$vivado_version" "$adi_revision" "$part" "$project_hash"
}

if [[ "$mode" == incremental ]]; then
  export FPGA_REFERENCE_META="$out/reference.dcp.meta"
  vivado_version="$(vivado -version | awk 'NR == 1 { print $2 }')"
  [[ -n "$vivado_version" ]] || { echo "could not read Vivado version" >&2; exit 1; }
  if FPGA_REFERENCE_KEY="$(reference_key)"; then
    export FPGA_INCREMENTAL=1 FPGA_REFERENCE_KEY
    reference_eligible=1
  else
    unset FPGA_INCREMENTAL FPGA_REFERENCE_KEY
    reference_eligible=0
  fi
  if ((dry_run)); then
    if ((!reference_eligible)); then
      echo "Dry run: ADI HDL checkout is dirty; this build cannot use or save an incremental reference."
    elif [[ -f "$out/reference.dcp" && -f "$out/reference.dcp.meta" ]] && \
       [[ "$(cat "$out/reference.dcp.meta")" == "$FPGA_REFERENCE_KEY" ]]; then
      echo "Dry run: incremental build of $name; compatible routed reference is available at $out/reference.dcp."
    else
      echo "Dry run: incremental build of $name; no compatible routed reference exists, so implementation starts without reuse."
    fi
    echo "Dry run: outputs would be rebuilt under $out; Vivado will not be launched."
    exit 0
  fi
else
  unset FPGA_INCREMENTAL FPGA_REFERENCE_META FPGA_REFERENCE_KEY
  if ((dry_run)); then
    echo "Dry run: clean build of $name under $out; Vivado will not be launched."
    exit 0
  fi
fi

mkdir -p "$out"
cd "$out"

if [[ "$mode" == clean ]]; then
  rm -f reference.dcp reference.dcp.meta
elif ((!reference_eligible)); then
  rm -f reference.dcp reference.dcp.meta
else
  reference_invalidated=0
  if [[ -f reference.dcp && -f reference.dcp.meta ]]; then
    if [[ "$(cat reference.dcp.meta)" != "$FPGA_REFERENCE_KEY" ]]; then
      echo "Discarding incremental reference: Vivado, ADI HDL, project, or part configuration changed."
      rm -f reference.dcp reference.dcp.meta
      reference_invalidated=1
    fi
  elif [[ -f reference.dcp || -f reference.dcp.meta ]]; then
    echo "Discarding incremental reference without matching metadata."
    rm -f reference.dcp reference.dcp.meta
    reference_invalidated=1
  fi

  if [[ ! -f reference.dcp && $reference_invalidated == 0 ]]; then
    previous_dcp="$out/$name.runs/impl_1/system_top_routed.dcp"
    previous_xpr="$out/$name.xpr"
    previous_log="$out/vivado.log"
    previous_timing="$out/timing_impl.log"
    previous_vivado="$(sed -n 's/^# Vivado \(v[^ ]*\).*/\1/p' "$previous_log" 2>/dev/null | head -1)"
    previous_wns="$(awk '/WNS\(ns\)/ { getline; getline; print $1; exit }' "$previous_timing" 2>/dev/null)"
    project_copy_matches=0
    if [[ -f "$out/system_project.tcl" ]] && cmp -s "$project_dir/system_project.tcl" "$out/system_project.tcl"; then
      project_copy_matches=1
    fi
    if [[ -s "$previous_dcp" && "$vivado_version" == "$previous_vivado" && \
          -n "$previous_wns" ]] && awk -v slack="$previous_wns" 'BEGIN { exit !(slack >= -0.250) }' && \
      ((project_copy_matches)) && rg -q "Name=\"Part\" Val=\"$part\"" "$previous_xpr"; then
      cp "$previous_dcp" reference.dcp
      printf '%s\n' "$FPGA_REFERENCE_KEY" >reference.dcp.meta
      echo "Using the previous routed checkpoint as the incremental reference (WNS ${previous_wns} ns)."
    else
      echo "No compatible routed reference found; this build will run without incremental implementation reuse."
    fi
  fi
fi

rsync -a --delete --exclude '*.runs' --exclude '*.cache' --exclude '*.gen' --exclude '*.srcs' \
  --exclude '*.hw' --exclude '*.ip_user_files' --exclude '*.sdk' --exclude '*.xpr' \
  --exclude '*.log' --exclude '*.jou' --exclude 'reports' \
  --exclude 'reference.dcp' --exclude 'reference.dcp.meta' "$project_dir/" "$out/"
for f in "$here"/rtl/*.v "$here"/rtl/mem/*.mem; do [ -e "$f" ] && cp "$f" "$out/"; done
rm -rf ./*.runs ./*.cache ./*.gen ./*.srcs ./*.hw ./*.ip_user_files ./*.sdk ./*.xpr .Xil
vivado -mode batch -nojournal -log vivado.log -source system_project.tcl
vivado -mode batch -nojournal -log reports.log -source "$here/scripts/reports.tcl" -tclargs "$name"
