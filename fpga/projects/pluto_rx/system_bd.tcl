# RX-only variant of ADI hdl projects/pluto (hdl_2026_r1).
#
# Builds the stock block design, then removes transmit features that the
# channelizer receiver does not use, to free LUTs and DSP48E1s on the xc7z010:
#   * axi_ad9361 DAC DDS tone generators        (DAC_DDS_DISABLE)
#   * axi_ad9361 DAC IQ correction              (DAC_IQCORRECTION_DISABLE)
#   * tx_fir_interpolator (x8 FIR for low TX rates) and its GPIO slice / valid OR
# The AD9363 interface, the whole RX path, the TX DMA + upack (kept so the stock
# device tree, which references the TX DMA and DDS core, still probes) and the
# TDD block are unchanged. TX through DMA remains possible at the native rate.

source $ad_hdl_dir/projects/pluto/system_bd.tcl

# Disconnect every pin of a cell, then delete it; nets left with fewer than two
# endpoints are deleted so the replacement connections start from clean pins.
proc rx_only_remove_cell {cell} {
  set touched {}
  foreach pin [get_bd_pins -quiet -of_objects [get_bd_cells $cell]] {
    set net [get_bd_nets -quiet -of_objects $pin]
    if {$net ne ""} {
      disconnect_bd_net $net $pin
      lappend touched $net
    }
  }
  delete_bd_objs [get_bd_cells $cell]
  foreach net [lsort -unique $touched] {
    if {[llength [get_bd_nets -quiet $net]] == 0} continue
    set ends [concat [get_bd_pins -quiet -of_objects $net] [get_bd_ports -quiet -of_objects $net]]
    if {[llength $ends] < 2} {
      delete_bd_objs $net
    }
  }
}

rx_only_remove_cell tx_fir_interpolator
rx_only_remove_cell interp_slice
rx_only_remove_cell logic_or

ad_ip_parameter axi_ad9361 CONFIG.DAC_DDS_DISABLE 1
ad_ip_parameter axi_ad9361 CONFIG.DAC_IQCORRECTION_DISABLE 1

# Channel 0 of the upack now feeds the core directly, like the other ADI
# AD9361 projects that have no interpolator (e.g. fmcomms2 via its DAC FIFO).
ad_connect axi_ad9361/dac_enable_i0 tx_upack/enable_0
ad_connect axi_ad9361/dac_enable_q0 tx_upack/enable_1
ad_connect tx_upack/fifo_rd_data_0 axi_ad9361/dac_data_i0
ad_connect tx_upack/fifo_rd_data_1 axi_ad9361/dac_data_q0
ad_connect axi_ad9361/dac_valid_i0 tx_upack/fifo_rd_en
