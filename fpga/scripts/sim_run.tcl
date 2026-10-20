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
set_property -name {xsim.elaborate.xelab.more_options} -value {-L xpm} -objects [get_filesets sim_1]
launch_simulation -simset sim_1 -mode behavioral
run all
close_sim
