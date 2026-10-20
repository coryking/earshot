# Create the xsim project for tb_chan (run once; sim_run.tcl reuses it).
#   vivado -mode batch -source sim_setup.tcl -tclargs <fpga dir> <sim dir> <adi hdl dir>
set fpga [file normalize [lindex $argv 0]]
set simdir [file normalize [lindex $argv 1]]
set adi [file normalize [lindex $argv 2]]
source $fpga/scripts/ip_xfft.tcl

create_project -force chan_sim $simdir -part xc7z010clg225-1
set_property target_language Verilog [current_project]
set_property simulator_language Mixed [current_project]
add_files [glob $fpga/rtl/*.v] $adi/library/common/up_axi.v
add_files [glob $fpga/rtl/mem/*.mem]
add_files -fileset sim_1 [glob $fpga/tb/*.sv]

create_ip -name xfft -vendor xilinx.com -library ip -module_name chan_xfft
set_property -dict [chan_xfft_config] [get_ips chan_xfft]

foreach {name bytes user} {axis_pc_fftin 4 0 axis_pc_fftout 8 16 axis_pc_hist 8 0 axis_pc_sel 8 0} {
  create_ip -name axis_protocol_checker -vendor xilinx.com -library ip -module_name $name
  set_property -dict [list CONFIG.TDATA_NUM_BYTES $bytes CONFIG.TUSER_WIDTH $user \
    CONFIG.HAS_TREADY 1 CONFIG.HAS_TLAST 1 CONFIG.HAS_TSTRB 0 CONFIG.HAS_TKEEP 0 \
    CONFIG.TID_WIDTH 0 CONFIG.TDEST_WIDTH 0 CONFIG.MAX_WAITS 0 CONFIG.ENABLE_CONTROL 0 CONFIG.ENABLE_MARK_DEBUG 0] \
    [get_ips $name]
}
create_ip -name axi_protocol_checker -vendor xilinx.com -library ip -module_name axi_pc_lite
set_property -dict [list CONFIG.PROTOCOL {AXI4LITE} CONFIG.ADDR_WIDTH {16} \
  CONFIG.DATA_WIDTH {32} CONFIG.HAS_SYSTEM_RESET {0} CONFIG.ENABLE_CONTROL {0} \
  CONFIG.ENABLE_MARK_DEBUG {0}] [get_ips axi_pc_lite]

generate_target {simulation instantiation_template} [get_ips]
set_property top tb_chan [get_filesets sim_1]
set_property -name {xsim.simulate.runtime} -value {all} -objects [get_filesets sim_1]
foreach ip [get_ips] { puts "IP [get_property CONFIG.Component_Name $ip]" }
close_project
