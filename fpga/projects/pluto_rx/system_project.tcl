# RX-only Pluto (xc7z010clg225-1) built from ADI hdl with this repo's overlay.
# ADI_HDL_DIR must point at the ADI hdl checkout (hdl_2026_r1, libraries built).
source $::env(ADI_HDL_DIR)/scripts/adi_env.tcl
source $ad_hdl_dir/projects/scripts/adi_project_xilinx.tcl
source $ad_hdl_dir/projects/scripts/adi_board.tcl

set pluto_dir $ad_hdl_dir/projects/pluto

adi_project_create pluto_rx 0 {} "xc7z010clg225-1"

adi_project_files pluto_rx [list \
  "$pluto_dir/system_top.v" \
  "$pluto_dir/system_constr.xdc" \
  "$ad_hdl_dir/library/common/ad_iobuf.v"]

set_property is_enabled false [get_files *system_sys_ps7_0.xdc]
adi_project_run pluto_rx
source $ad_hdl_dir/library/axi_ad9361/axi_ad9361_delay.tcl
