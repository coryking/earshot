# Post-route sign-off reports for a built project (run from its build dir).
set name [lindex $argv 0]
open_checkpoint ${name}.runs/impl_1/system_top_routed.dcp
file mkdir reports
report_utilization -file reports/utilization.rpt
report_utilization -hierarchical -hierarchical_depth 4 -file reports/utilization_hier.rpt
report_timing_summary -max_paths 20 -report_unconstrained -file reports/timing_summary.rpt
report_methodology -file reports/methodology.rpt
report_drc -file reports/drc.rpt
report_cdc -details -file reports/cdc.rpt
check_timing -verbose -file reports/check_timing.rpt
report_clock_interaction -file reports/clock_interaction.rpt
report_exceptions -file reports/exceptions.rpt
report_pulse_width -file reports/pulse_width.rpt
