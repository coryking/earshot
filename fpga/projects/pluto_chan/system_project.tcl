# Pluto RX-only + channelizer (xc7z010clg225-1), built from ADI hdl with this
# repo's overlay. ADI_HDL_DIR must point at the ADI hdl checkout.
source $::env(ADI_HDL_DIR)/scripts/adi_env.tcl
source $ad_hdl_dir/projects/scripts/adi_project_xilinx.tcl
source $ad_hdl_dir/projects/scripts/adi_board.tcl

set pluto_dir $ad_hdl_dir/projects/pluto

adi_project_create pluto_chan 0 {} "xc7z010clg225-1"

adi_project_files pluto_chan [list \
  "$pluto_dir/system_top.v" \
  "$pluto_dir/system_constr.xdc" \
  "$ad_hdl_dir/library/common/ad_iobuf.v"]

set_property is_enabled false [get_files *system_sys_ps7_0.xdc]
adi_project_run pluto_chan

# Keep a timing-qualified routed checkpoint for the next opt-in incremental
# build. ADI's project Tcl consumes ./reference.dcp when creating impl_1.
if {[info exists ::env(FPGA_INCREMENTAL)] && $::env(FPGA_INCREMENTAL) eq "1"} {
  if {![info exists ::env(FPGA_REFERENCE_META)] || ![info exists ::env(FPGA_REFERENCE_KEY)]} {
    error "incremental build is missing reference checkpoint metadata"
  }
  set reference_dcp [file normalize ./reference.dcp]
  set routed_dcp [file normalize ./pluto_chan.runs/impl_1/system_top_routed.dcp]
  set setup_paths [get_timing_paths -setup -max_paths 1]
  if {[llength $setup_paths] > 0 && [file isfile $routed_dcp]} {
    set worst_slack [get_property SLACK [lindex $setup_paths 0]]
    if {$worst_slack >= -0.250} {
      file copy -force $routed_dcp $reference_dcp
      set meta_tmp "$::env(FPGA_REFERENCE_META).tmp"
      set meta_file [open $meta_tmp w]
      puts $meta_file $::env(FPGA_REFERENCE_KEY)
      close $meta_file
      file rename -force $meta_tmp $::env(FPGA_REFERENCE_META)
      puts "Saved incremental reference checkpoint (WNS ${worst_slack} ns)."
    } else {
      puts "Keeping the prior incremental reference: WNS ${worst_slack} ns is below -0.250 ns."
    }
  } else {
    puts "Keeping the prior incremental reference: no setup path or routed checkpoint was available."
  }
}

source $ad_hdl_dir/library/axi_ad9361/axi_ad9361_delay.tcl
