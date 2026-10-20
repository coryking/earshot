# The 512-point xfft configuration shared by simulation and the block design, and
# mirrored by fpga/model/channelizer.py Config (fft_in_bits, twiddle_bits, rounding).
# PG109 v9.1: pipelined streaming I/O sustains one sample per clock, so a 512-bin
# frame takes 512 of the 1000 clocks available per 256-sample hop at 100 MHz.
# Bit-reversed output order (XK_INDEX carries k) avoids the reorder buffer; every
# consumer downstream addresses by k. 16-bit input and four block-RAM stages are
# the smallest configuration measured (2,461 LUT / 22 DSP vs 2,969 / 24).
proc chan_xfft_config {} {
  return [list \
    CONFIG.transform_length {512} \
    CONFIG.implementation_options {pipelined_streaming_io} \
    CONFIG.data_format {fixed_point} \
    CONFIG.input_width {16} \
    CONFIG.phase_factor_width {18} \
    CONFIG.scaling_options {unscaled} \
    CONFIG.rounding_modes {convergent_rounding} \
    CONFIG.output_ordering {bit_reversed_order} \
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
    CONFIG.memory_options_reorder {block_ram} \
    CONFIG.number_of_stages_using_block_ram_for_data_and_phase_factors {4}]
}
