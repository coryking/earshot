# Pluto RX-only design plus the coarse channelizer.
#
# axi_ad9361 channel 0 (l_clk) -> chan_top (CDC, PFB FIR) -> xfft -> chan_top
# (phase correction, PSD/activity, packetizer) -> two ADI axi_dmac:
#   chan_hist_dma: every bin, cyclic ring in DDR (HP0)
#   chan_sel_dma : selected bins (HP3)
# The stock RX path (rx_fir_decimator -> cpack -> axi_ad9361_adc_dma) is untouched,
# so libiio capture keeps working alongside the channelizer.

source $::env(FPGA_DIR)/projects/pluto_rx/system_bd.tcl
source $::env(FPGA_DIR)/scripts/ip_xfft.tcl

# module reference sources must be in the project before the cell is created
add_files -norecurse [glob ./chan_*.v] $ad_hdl_dir/library/common/up_axi.v
add_files -norecurse [glob ./*.mem]

create_bd_cell -type module -reference chan_top chan
set_property -dict [list CONFIG.ID {1}] [get_bd_cells chan]

ad_ip_instance xfft chan_fft
set_property -dict [chan_xfft_config] [get_bd_cells chan_fft]

foreach {name cyclic} {chan_hist_dma 1 chan_sel_dma 0} {
  ad_ip_instance axi_dmac $name
  ad_ip_parameter $name CONFIG.DMA_TYPE_SRC 1
  ad_ip_parameter $name CONFIG.DMA_TYPE_DEST 0
  ad_ip_parameter $name CONFIG.CYCLIC $cyclic
  ad_ip_parameter $name CONFIG.SYNC_TRANSFER_START 0
  ad_ip_parameter $name CONFIG.AXI_SLICE_SRC 0
  ad_ip_parameter $name CONFIG.AXI_SLICE_DEST 0
  ad_ip_parameter $name CONFIG.DMA_2D_TRANSFER 0
  ad_ip_parameter $name CONFIG.DMA_DATA_WIDTH_SRC 64
  ad_ip_parameter $name CONFIG.DMA_DATA_WIDTH_DEST 64
}

# ADC tap (same signals that feed rx_fir_decimator channel 0)
ad_connect axi_ad9361/l_clk chan/adc_clk
ad_connect axi_ad9361/rst chan/adc_rst
ad_connect axi_ad9361/adc_valid_i0 chan/adc_valid
ad_connect axi_ad9361/adc_data_i0 chan/adc_data_i
ad_connect axi_ad9361/adc_data_q0 chan/adc_data_q

# FFT
ad_connect sys_cpu_clk chan_fft/aclk
ad_connect chan/fft_aresetn chan_fft/aresetn
ad_connect chan/m_axis_fft chan_fft/S_AXIS_DATA
ad_connect chan/m_axis_fftcfg chan_fft/S_AXIS_CONFIG
ad_connect chan_fft/M_AXIS_DATA chan/s_axis_fftout
ad_connect chan_fft/event_tlast_unexpected chan/fft_event_tlast_unexpected
ad_connect chan_fft/event_tlast_missing chan/fft_event_tlast_missing
ad_connect chan_fft/event_data_in_channel_halt chan/fft_event_data_in_channel_halt

# streams to the DMAs (everything on sys_cpu_clk: no further clock crossings)
ad_connect chan/m_axis_hist chan_hist_dma/s_axis
ad_connect chan/m_axis_sel chan_sel_dma/s_axis
foreach name {chan_hist_dma chan_sel_dma} {
  ad_connect sys_cpu_clk $name/s_axis_aclk
  ad_connect sys_cpu_clk $name/m_dest_axi_aclk
  ad_connect sys_cpu_resetn $name/m_dest_axi_aresetn
}

ad_ip_parameter sys_ps7 CONFIG.PCW_USE_S_AXI_HP0 {1}
ad_ip_parameter sys_ps7 CONFIG.PCW_USE_S_AXI_HP3 {1}
ad_connect sys_cpu_clk sys_ps7/S_AXI_HP0_ACLK
ad_connect sys_cpu_clk sys_ps7/S_AXI_HP3_ACLK
ad_connect chan_hist_dma/m_dest_axi sys_ps7/S_AXI_HP0
ad_connect chan_sel_dma/m_dest_axi sys_ps7/S_AXI_HP3
create_bd_addr_seg -range 0x20000000 -offset 0x00000000 \
  [get_bd_addr_spaces chan_hist_dma/m_dest_axi] \
  [get_bd_addr_segs sys_ps7/S_AXI_HP0/HP0_DDR_LOWOCM] SEG_sys_ps7_HP0_DDR_LOWOCM
create_bd_addr_seg -range 0x20000000 -offset 0x00000000 \
  [get_bd_addr_spaces chan_sel_dma/m_dest_axi] \
  [get_bd_addr_segs sys_ps7/S_AXI_HP3/HP3_DDR_LOWOCM] SEG_sys_ps7_HP3_DDR_LOWOCM

ad_cpu_interconnect 0x7C460000 chan_hist_dma
ad_cpu_interconnect 0x7C470000 chan_sel_dma
ad_cpu_interconnect 0x7C480000 chan
ad_connect sys_cpu_resetn chan/s_axi_aresetn

ad_cpu_interrupt ps-10 mb-10 chan_hist_dma/irq
ad_cpu_interrupt ps-9 mb-9 chan_sel_dma/irq
