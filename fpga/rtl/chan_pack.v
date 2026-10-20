// Frame packetizer: history stream (every bin) and selected-bin stream.
//
// Both are 64-bit AXI4-Stream masters (TLAST unused, tied low) feeding ADI
// axi_dmac instances (DMA_TYPE_SRC = 1). Little-endian word formats:
//
//   header    [63:60]=0xA [59:56]=type (1 hist, 2 sel) [55:48]=flags [47:0]=frame
//   hist data [15:0]=I(2p) [31:16]=Q(2p) [47:32]=I(2p+1) [63:48]=Q(2p+1), p=0..255
//   sel data  [63:60]=0x5 [56:48]=bin [47:32]=frame[15:0] [31:16]=Q [15:0]=I
//   flags: bit0 frames of this stream were dropped before this one
//          bit1 this frame's 4096-sample window may span lost ADC samples (CDC FIFO
//               overflow or ADC core reset): set for the 16 frames after a loss
//          bit2 a sample in this frame saturated the 16-bit output
//
// Admission is per frame: a frame enters a stream only if that stream's FIFO has
// room for the whole record (257 / 513 words), so records are never truncated and
// a stalled DMA costs whole, counted frames instead of backpressure upstream.
// Bin order is natural FFT order k = 0..511 (k >= 256 are negative offsets).

`timescale 1ns/1ps
`default_nettype none

module chan_pack #(
  parameter FIFO_DEPTH = 1024
) (
  input  wire         clk,
  input  wire         rst,          // soft reset: control state, counters
  input  wire         fifo_rst,     // system reset only: FIFOs keep draining to the
                                    // DMA across a soft reset (AXI4-Stream TVALID
                                    // must not drop before its handshake)

  input  wire         fb_we,
  input  wire         fb_bank,
  input  wire [8:0]   fb_addr,
  input  wire [31:0]  fb_data,
  input  wire         fd_valid,
  input  wire [47:0]  fd_frame,
  input  wire         fd_sat,

  input  wire         hist_en,
  input  wire         sel_en,
  input  wire         auto_forward,
  output wire [8:0]   sel_query,      // bin whose selection bits are needed now
  input  wire         mask_bit,       // FORWARD_MASK[sel_query] (asynchronous read)
  input  wire         active_bit,     // ACTIVE[sel_query] (asynchronous read)
  input  wire [31:0]  adc_overflow_count,

  output wire         m_hist_valid,
  output wire [63:0]  m_hist_data,
  input  wire         m_hist_ready,
  output wire         m_hist_last,
  output wire         m_sel_valid,
  output wire [63:0]  m_sel_data,
  input  wire         m_sel_ready,
  output wire         m_sel_last,

  output reg  [31:0]  hist_frames = 32'd0,
  output reg  [31:0]  hist_drops = 32'd0,
  output reg  [47:0]  hist_first_frame = 48'd0,
  output reg  [31:0]  sel_frames = 32'd0,
  output reg  [31:0]  sel_drops = 32'd0,
  output reg  [31:0]  overruns = 32'd0,
  output wire         busy_o,
  output wire         hist_empty_o,
  output wire         sel_empty_o,
  // frame-buffer readback for the live self-test (valid while not busy)
  input  wire [9:0]   snap_addr,      // {bank, bin}
  output wire [31:0]  snap_data       // latency 1
);

  localparam CW = $clog2(FIFO_DEPTH) + 1;

  // ---------------------------------------------------------------------------
  // frame buffer: 2 banks x 512 x 32
  (* ram_style = "block" *) reg [31:0] fbuf [0:1023];
  reg  [9:0]  rd_addr;
  reg  [31:0] rd_data;
  always @(posedge clk) begin
    if (fb_we) fbuf[{fb_bank, fb_addr}] <= fb_data;
    rd_data <= fbuf[rd_addr];
  end

  // ---------------------------------------------------------------------------
  // frame queue (one pending frame) and sequencer
  reg         busy = 1'b0;
  reg         pend = 1'b0;
  reg  [47:0] pend_frame;
  reg         pend_sat;
  reg  [47:0] cur_frame;
  reg         cur_bank;
  reg         cur_sat;
  reg  [9:0]  b = 10'd0;          // bin being read (0..511), 512 = done
  reg         hist_go, sel_go;
  reg         hist_disc = 1'b0, sel_disc = 1'b0;
  reg  [4:0]  ovf_frames = 5'd0;  // frames still overlapping a sample loss
  reg  [31:0] ovf_seen = 32'd0;
  reg         hist_armed = 1'b0;   // first frame after enable not yet recorded

  wire [CW-1:0] hist_count, sel_count;
  wire hist_room = hist_count <= FIFO_DEPTH - 257;
  wire sel_room  = sel_count  <= FIFO_DEPTH - 513;

  // FIFO write ports
  reg         hw_en = 1'b0, sw_en = 1'b0;
  reg  [63:0] hw_data, sw_data;

  // read pipeline: address at cycle c, data at c+1
  reg         rv = 1'b0;
  reg  [8:0]  rb;
  reg  [31:0] even_word;

  wire start = ~busy & ~rst & (pend | fd_valid);
  wire [47:0] start_frame = pend ? pend_frame : fd_frame;
  wire        start_sat   = pend ? pend_sat : fd_sat;
  wire        start_bank  = start_frame[0];
  wire        ovf_new     = (adc_overflow_count != ovf_seen);
  wire        ovf_flag    = ovf_new | (ovf_frames != 5'd0);

  assign sel_query = rb;
  wire sel_bit = mask_bit | (auto_forward & active_bit);

  // A soft reset (RUN = 0) waits for the record being written to finish, so the
  // output streams never carry a truncated record (the history ring's fixed
  // record size depends on it). New frames are not admitted meanwhile.
  wire rst_now = rst & ~busy;
  always @(posedge clk) begin
    if (rst_now) begin
      busy <= 1'b0; pend <= 1'b0; b <= 10'd0; rv <= 1'b0;
      hw_en <= 1'b0; sw_en <= 1'b0;
      hist_disc <= 1'b0; sel_disc <= 1'b0; ovf_frames <= 5'd0;
      ovf_seen <= adc_overflow_count; hist_armed <= 1'b1;
      hist_frames <= 32'd0; hist_drops <= 32'd0; sel_frames <= 32'd0;
      sel_drops <= 32'd0; overruns <= 32'd0; hist_first_frame <= 48'd0;
      hist_go <= 1'b0; sel_go <= 1'b0;
    end else begin
      hw_en <= 1'b0;
      sw_en <= 1'b0;

      // A loss affects every frame whose 4096-sample window (16 hops) spans it.
      if (ovf_new) begin
        ovf_seen <= adc_overflow_count;
        ovf_frames <= 5'd16;
      end else if (start && ovf_frames != 5'd0) begin
        ovf_frames <= ovf_frames - 5'd1;
      end
      if (!hist_en) hist_armed <= 1'b1;

      // queue a frame that arrives while busy; a third one is an overrun
      if (fd_valid & ~start & ~rst) begin
        if (pend) begin
          overruns <= overruns + 32'd1;
          hist_disc <= hist_disc | hist_en;
          sel_disc <= sel_disc | sel_en;
        end else begin
          pend <= 1'b1;
          pend_frame <= fd_frame;
          pend_sat <= fd_sat;
        end
      end

      if (start) begin
        if (pend) begin
          pend <= fd_valid;            // a new arrival refills the slot
          pend_frame <= fd_frame;
          pend_sat <= fd_sat;
        end
        busy <= 1'b1;
        cur_frame <= start_frame;
        cur_bank <= start_bank;
        cur_sat <= start_sat;
        b <= 10'd0;
        hist_go <= hist_en & hist_room;
        sel_go <= sel_en & sel_room;
        if (hist_en & hist_room) begin
          hw_en <= 1'b1;
          hw_data <= {4'hA, 4'h1, 5'd0, start_sat, ovf_flag, hist_disc, start_frame};
          hist_disc <= 1'b0;
          hist_frames <= hist_frames + 32'd1;
          if (hist_armed) begin
            hist_first_frame <= start_frame;
            hist_armed <= 1'b0;
          end
        end else if (hist_en) begin
          hist_drops <= hist_drops + 32'd1;
          hist_disc <= 1'b1;
        end
        if (sel_en & sel_room) begin
          sw_en <= 1'b1;
          sw_data <= {4'hA, 4'h2, 5'd0, start_sat, ovf_flag, sel_disc, start_frame};
          sel_disc <= 1'b0;
          sel_frames <= sel_frames + 32'd1;
        end else if (sel_en) begin
          sel_drops <= sel_drops + 32'd1;
          sel_disc <= 1'b1;
        end
      end else if (busy) begin
        if (b != 10'd512) b <= b + 10'd1;
        else if (!rv) busy <= 1'b0;
      end

      // read pipeline
      rv <= busy & ~start & (b != 10'd512);
      rb <= b[8:0];
      if (rv) begin
        if (!rb[0]) even_word <= rd_data;
        if (rb[0] & hist_go) begin
          hw_en <= 1'b1;
          hw_data <= {rd_data, even_word};
        end
        if (sel_go & sel_bit) begin
          sw_en <= 1'b1;
          sw_data <= {4'h5, 3'd0, rb, cur_frame[15:0], rd_data};
        end
      end
    end
  end

  always @(*) rd_addr = busy ? {cur_bank, b[8:0]} : snap_addr;
  assign snap_data = rd_data;
  assign busy_o = busy | pend;

  // ---------------------------------------------------------------------------
  // output FIFOs (xpm_fifo_sync, first-word fall-through)
  wire hist_empty, sel_empty;
  xpm_fifo_sync #(
    .DOUT_RESET_VALUE ("0"), .ECC_MODE ("no_ecc"), .FIFO_MEMORY_TYPE ("block"),
    .FIFO_READ_LATENCY (0), .FIFO_WRITE_DEPTH (FIFO_DEPTH), .FULL_RESET_VALUE (0),
    .PROG_EMPTY_THRESH (10), .PROG_FULL_THRESH (10), .RD_DATA_COUNT_WIDTH (CW),
    .READ_DATA_WIDTH (64), .READ_MODE ("fwft"), .SIM_ASSERT_CHK (0),
    .USE_ADV_FEATURES ("0004"), .WAKEUP_TIME (0), .WRITE_DATA_WIDTH (64),
    .WR_DATA_COUNT_WIDTH (CW)
  ) i_hist_fifo (
    .rst (fifo_rst), .wr_clk (clk), .wr_en (hw_en), .din (hw_data), .full (),
    .wr_data_count (hist_count), .rd_en (m_hist_valid & m_hist_ready),
    .dout (m_hist_data), .empty (hist_empty), .sleep (1'b0), .injectsbiterr (1'b0),
    .injectdbiterr (1'b0), .almost_empty (), .almost_full (), .data_valid (),
    .dbiterr (), .overflow (), .prog_empty (), .prog_full (), .rd_data_count (),
    .rd_rst_busy (), .sbiterr (), .underflow (), .wr_ack (), .wr_rst_busy ()
  );
  xpm_fifo_sync #(
    .DOUT_RESET_VALUE ("0"), .ECC_MODE ("no_ecc"), .FIFO_MEMORY_TYPE ("block"),
    .FIFO_READ_LATENCY (0), .FIFO_WRITE_DEPTH (FIFO_DEPTH), .FULL_RESET_VALUE (0),
    .PROG_EMPTY_THRESH (10), .PROG_FULL_THRESH (10), .RD_DATA_COUNT_WIDTH (CW),
    .READ_DATA_WIDTH (64), .READ_MODE ("fwft"), .SIM_ASSERT_CHK (0),
    .USE_ADV_FEATURES ("0004"), .WAKEUP_TIME (0), .WRITE_DATA_WIDTH (64),
    .WR_DATA_COUNT_WIDTH (CW)
  ) i_sel_fifo (
    .rst (fifo_rst), .wr_clk (clk), .wr_en (sw_en), .din (sw_data), .full (),
    .wr_data_count (sel_count), .rd_en (m_sel_valid & m_sel_ready),
    .dout (m_sel_data), .empty (sel_empty), .sleep (1'b0), .injectsbiterr (1'b0),
    .injectdbiterr (1'b0), .almost_empty (), .almost_full (), .data_valid (),
    .dbiterr (), .overflow (), .prog_empty (), .prog_full (), .rd_data_count (),
    .rd_rst_busy (), .sbiterr (), .underflow (), .wr_ack (), .wr_rst_busy ()
  );

  assign hist_empty_o = hist_empty;
  assign sel_empty_o = sel_empty;
  assign m_hist_valid = ~hist_empty;
  assign m_sel_valid = ~sel_empty;
  assign m_hist_last = 1'b0;
  assign m_sel_last = 1'b0;

endmodule

`default_nettype wire
