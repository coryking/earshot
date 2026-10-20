# Run tb_chan for one scenario in the project made by sim_setup.tcl.
#   vivado -mode batch -source sim_run.tcl -tclargs <sim dir> <vector dir>
set simdir [file normalize [lindex $argv 0]]
set vec [file normalize [lindex $argv 1]]
open_project $simdir/chan_sim.xpr
set fh [open $vec/plusargs.txt r]
set args [string trim [read $fh]]
close $fh
set opts "-testplusarg VEC=$vec"
foreach a $args { append opts " -testplusarg [string range $a 1 end]" }
set_property -name {xsim.simulate.xsim.more_options} -value $opts -objects [get_filesets sim_1]
# TB_FIR2X=0 in the environment selects the 16-DSP FIR (tb_chan parameter FIR2X)
set xelab_opts {-L xpm}
if {[info exists ::env(TB_FIR2X)]} { append xelab_opts " -generic_top FIR2X=$::env(TB_FIR2X)" }
set_property -name {xsim.elaborate.xelab.more_options} -value $xelab_opts -objects [get_filesets sim_1]
launch_simulation -simset sim_1 -mode behavioral
run all
close_sim
