# Out-of-context synthesis sweep of the xfft v9.1 core (resource cost of each knob).
#
#   vivado -mode batch -source fpga/scripts/xfft_sweep.tcl -tclargs <outdir> [name ...]
#
# Starts from ip_xfft.tcl's chan_xfft_config (the configuration the channelizer
# uses), applies each named override set below, runs synth_ip, and appends one row
# per run to <outdir>/summary.csv: name,LUT,LUTRAM,FF,RAMB36,RAMB18,DSP,wns_note.
# With no names, every set is run. Runs are sequential in one process.
set here [file dirname [file normalize [info script]]]
source $here/ip_xfft.tcl

set outdir [file normalize [lindex $argv 0]]
set names   [lrange $argv 1 end]
file mkdir $outdir

# name -> CONFIG overrides on top of chan_xfft_config.
set sets [dict create \
  n512_base        {} \
  n512_pf17        {CONFIG.phase_factor_width 17} \
  n512_pf16        {CONFIG.phase_factor_width 16} \
  n512_mult4       {CONFIG.complex_mult_type use_mults_performance} \
  n512_multluts    {CONFIG.complex_mult_type use_luts} \
  n512_bflydsp     {CONFIG.butterfly_type use_xtremedsp_slices} \
  n512_pf16_multluts {CONFIG.phase_factor_width 16 CONFIG.complex_mult_type use_luts} \
  n512_pf17_multluts {CONFIG.phase_factor_width 17 CONFIG.complex_mult_type use_luts} \
  n512_pf16_mult4  {CONFIG.phase_factor_width 16 CONFIG.complex_mult_type use_mults_performance} \
  n512_pf16_bflydsp {CONFIG.phase_factor_width 16 CONFIG.butterfly_type use_xtremedsp_slices} \
  n512_multluts_bflydsp {CONFIG.complex_mult_type use_luts CONFIG.butterfly_type use_xtremedsp_slices} \
  n1024_base       {CONFIG.transform_length 1024} \
  n1024_b5         {CONFIG.transform_length 1024 CONFIG.number_of_stages_using_block_ram_for_data_and_phase_factors 5} \
  n1024_pf17       {CONFIG.transform_length 1024 CONFIG.phase_factor_width 17} \
  n1024_pf16       {CONFIG.transform_length 1024 CONFIG.phase_factor_width 16} \
  n1024_multluts   {CONFIG.transform_length 1024 CONFIG.complex_mult_type use_luts} \
  n1024_mult4      {CONFIG.transform_length 1024 CONFIG.complex_mult_type use_mults_performance} \
]
if {[llength $names] == 0} { set names [dict keys $sets] }

cd $outdir
foreach name $names {
  if {[file exists f_$name/f_$name.dcp]} { puts "skip $name (done)"; continue }
  create_project -in_memory -part xc7z010clg225-1
  create_ip -name xfft -vendor xilinx.com -library ip -module_name f_$name -dir .
  set_property -dict [concat [chan_xfft_config] [dict get $sets $name]] [get_ips f_$name]
  generate_target all [get_ips f_$name]
  synth_ip [get_ips f_$name]
  open_checkpoint f_$name/f_$name.dcp
  set rpt util_$name.rpt
  report_utilization -file $rpt
  set fh [open $rpt]; set txt [read $fh]; close $fh
  proc grab {pat txt} {
    if {[regexp "\\|\\s*$pat\\s*\\|\\s*(\\d+)" $txt -> v]} { return $v } else { return NA }
  }
  set row [list $name [grab {Slice LUTs\*?} $txt] [grab {LUT as Memory} $txt] \
    [grab {Slice Registers} $txt] [grab {Block RAM Tile} $txt] [grab {RAMB36/FIFO\*?} $txt] \
    [grab {RAMB18} $txt] [grab {DSPs} $txt]]
  set sf [open summary.csv a]; puts $sf [join $row ,]; close $sf
  close_design
  close_project
}
