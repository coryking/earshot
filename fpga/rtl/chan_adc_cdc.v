// ADC sample clock-domain crossing for the channelizer.
//
// adc_clk (axi_ad9361 l_clk, rate depends on the AD9363 sample rate) -> clk (the
// fixed channelizer clock). One xpm_fifo_async carries {Q,I}; per UG949 the CDC is
// an XPM macro, never ad-hoc logic. The FIFO is reset by the ADC core reset, which
// is synchronous to adc_clk as xpm_fifo_async requires.
//
// Samples are written whenever axi_ad9361 marks channel 0 valid and the proc side
// has requested capture (run, synchronized into adc_clk). If the FIFO is full the
// sample is dropped and counted; the count crosses back as a Gray-coded bus
// (xpm_cdc_gray). A non-zero increment is a hardware-detected discontinuity.

`timescale 1ns/1ps
`default_nettype none

module chan_adc_cdc #(
  parameter FIFO_DEPTH = 512
) (
  input  wire        adc_clk,
  input  wire        adc_rst,
  input  wire        adc_valid,
  input  wire [15:0] adc_data_i,
  input  wire [15:0] adc_data_q,

  input  wire        clk,
  input  wire        run,             // clk domain level
  output wire        m_valid,
  output wire [15:0] m_i,
  output wire [15:0] m_q,
  input  wire        m_ready,
  output wire [31:0] overflow_count   // clk domain, Gray-synchronized
);

  wire        run_adc;
  wire        full;
  wire        wr_rst_busy;
  wire        empty;
  wire        rd_rst_busy;
  wire [31:0] dout;
  reg  [31:0] overflow_adc = 32'd0;

  xpm_cdc_single #(
    .DEST_SYNC_FF   (3),
    .INIT_SYNC_FF   (0),
    .SIM_ASSERT_CHK (0),
    .SRC_INPUT_REG  (1)
  ) i_run_sync (
    .src_clk  (clk),
    .src_in   (run),
    .dest_clk (adc_clk),
    .dest_out (run_adc)
  );

  wire wr_en = adc_valid & run_adc & ~full & ~wr_rst_busy;

  // Control-path counter: no reset needed beyond the ADC core reset.
  always @(posedge adc_clk) begin
    if (adc_rst) begin
      overflow_adc <= 32'd0;
    end else if (adc_valid & run_adc & (full | wr_rst_busy)) begin
      overflow_adc <= overflow_adc + 32'd1;
    end
  end

  xpm_fifo_async #(
    .CDC_SYNC_STAGES     (3),
    .DOUT_RESET_VALUE    ("0"),
    .ECC_MODE            ("no_ecc"),
    .FIFO_MEMORY_TYPE    ("block"),
    .FIFO_READ_LATENCY   (0),
    .FIFO_WRITE_DEPTH    (FIFO_DEPTH),
    .FULL_RESET_VALUE    (1),
    .PROG_EMPTY_THRESH   (10),
    .PROG_FULL_THRESH    (10),
    .RD_DATA_COUNT_WIDTH (1),
    .READ_DATA_WIDTH     (32),
    .READ_MODE           ("fwft"),
    .RELATED_CLOCKS      (0),
    .SIM_ASSERT_CHK      (0),
    .USE_ADV_FEATURES    ("0000"),
    .WAKEUP_TIME         (0),
    .WRITE_DATA_WIDTH    (32),
    .WR_DATA_COUNT_WIDTH (1)
  ) i_fifo (
    .rst           (adc_rst),
    .wr_clk        (adc_clk),
    .wr_en         (wr_en),
    .din           ({adc_data_q, adc_data_i}),
    .full          (full),
    .wr_rst_busy   (wr_rst_busy),
    .rd_clk        (clk),
    .rd_en         (m_valid & m_ready),
    .dout          (dout),
    .empty         (empty),
    .rd_rst_busy   (rd_rst_busy),
    .sleep         (1'b0),
    .injectsbiterr (1'b0),
    .injectdbiterr (1'b0),
    .almost_empty  (),
    .almost_full   (),
    .data_valid    (),
    .dbiterr       (),
    .overflow      (),
    .prog_empty    (),
    .prog_full     (),
    .rd_data_count (),
    .sbiterr       (),
    .underflow     (),
    .wr_ack        (),
    .wr_data_count ()
  );

  assign m_valid = ~empty & ~rd_rst_busy;
  assign m_i = dout[15:0];
  assign m_q = dout[31:16];

  xpm_cdc_gray #(
    .DEST_SYNC_FF          (3),
    .INIT_SYNC_FF          (0),
    .REG_OUTPUT            (1),
    .SIM_ASSERT_CHK        (0),
    .SIM_LOSSLESS_GRAY_CHK (0),
    .WIDTH                 (32)
  ) i_overflow_sync (
    .src_clk      (adc_clk),
    .src_in_bin   (overflow_adc),
    .dest_clk     (clk),
    .dest_out_bin (overflow_count)
  );

endmodule

`default_nettype wire
