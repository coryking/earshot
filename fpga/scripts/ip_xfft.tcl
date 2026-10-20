# The 512-point xfft configuration shared by simulation and the block design.
# PG109 v9.1: pipelined streaming I/O sustains one sample per clock, so a 512-bin
# frame takes 512 of the 1000 clocks available per 256-sample hop at 100 MHz.
proc chan_xfft_config {} {
  return [list \
    CONFIG.transform_length {512} \
    CONFIG.implementation_options {pipelined_streaming_io} \
    CONFIG.data_format {fixed_point} \
    CONFIG.input_width {18} \
    CONFIG.phase_factor_width {18} \
    CONFIG.scaling_options {unscaled} \
    CONFIG.rounding_modes {convergent_rounding} \
    CONFIG.output_ordering {natural_order} \
    CONFIG.xk_index {true} \
    CONFIG.throttle_scheme {nonrealtime} \
    CONFIG.aresetn {true} \
    CONFIG.ovflo {false} \
    CONFIG.run_time_configurable_transform_length {false} \
    CONFIG.target_clock_frequency {100} \
    CONFIG.complex_mult_type {use_mults_resources} \
    CONFIG.butterfly_type {use_luts} \
    CONFIG.memory_options_data {block_ram} \
    CONFIG.memory_options_phase_factors {block_ram} \
    CONFIG.memory_options_reorder {block_ram}]
}
