// Pluto coarse channelizer: 512-bin, 2x-oversampled polyphase filter bank with
// per-bin PSD/activity and two AXI4-Stream outputs for ADI axi_dmac.
//
// Everything except the ADC side of the CDC FIFO runs on s_axi_aclk (the PS
// FCLK0, 100 MHz in the Pluto design). The 512-point FFT is the AMD xfft core,
// instantiated next to this module in the block design and connected through the
// m_axis_fft / s_axis_fftout ports; fft_aresetn resets it together with the
// datapath so neither side of that stream is left mid-transfer.
//
// Registers use the ADI up_axi slave and the ADI convention of DWORD addresses with
// VERSION/ID/SCRATCH at 0x0/0x1/0x2. Full map: fpga/docs/design.md.

`timescale 1ns/1ps
`default_nettype none

module chan_top #(
  parameter ID = 0,
  parameter COEF_FILE_PREFIX = "pfb_coef_p",
  parameter LOG_TABLE = "chan_log2_table.mem",
  // CFAR thresholds above the minimum-statistics floor (Q8 log2): 2.60 dB gives
  // Pfa 1e-8 per bin-block and 1.57 dB gives 1e-2 (fpga/model/results/activity.json)
  parameter [15:0] DEFAULT_ON_Q8 = 16'd221,
  parameter [15:0] DEFAULT_OFF_Q8 = 16'd133,
  parameter [15:0] DEFAULT_BIAS_Q8 = 16'd0,
  parameter [7:0]  DEFAULT_HANG = 8'd39,
  parameter BLOCK_LOG2 = 8,             // detector block = 2^8 frames (2.56 ms)
  parameter SUB_BLOCKS = 128,           // minimum-statistics sub-window, blocks
  parameter PSD_LOG2 = 13               // long PSD = 2^13 frames (81.9 ms)
) (
  // AXI4-Lite (ADI up_axi), clock and reset for the whole core
  (* X_INTERFACE_INFO = "xilinx.com:signal:clock:1.0 s_axi_aclk CLK" *)
  (* X_INTERFACE_PARAMETER = "ASSOCIATED_BUSIF s_axi:m_axis_fft:m_axis_fftcfg:s_axis_fftout:m_axis_hist:m_axis_sel, ASSOCIATED_RESET s_axi_aresetn:fft_aresetn" *)
  input  wire         s_axi_aclk,
  (* X_INTERFACE_INFO = "xilinx.com:signal:reset:1.0 s_axi_aresetn RST" *)
  (* X_INTERFACE_PARAMETER = "POLARITY ACTIVE_LOW" *)
  input  wire         s_axi_aresetn,
  input  wire         s_axi_awvalid,
  input  wire [15:0]  s_axi_awaddr,
  input  wire [2:0]   s_axi_awprot,
  output wire         s_axi_awready,
  input  wire         s_axi_wvalid,
  input  wire [31:0]  s_axi_wdata,
  input  wire [3:0]   s_axi_wstrb,
  output wire         s_axi_wready,
  output wire         s_axi_bvalid,
  output wire [1:0]   s_axi_bresp,
  input  wire         s_axi_bready,
  input  wire         s_axi_arvalid,
  input  wire [15:0]  s_axi_araddr,
  input  wire [2:0]   s_axi_arprot,
  output wire         s_axi_arready,
  output wire         s_axi_rvalid,
  output wire [1:0]   s_axi_rresp,
  output wire [31:0]  s_axi_rdata,
  input  wire         s_axi_rready,

  // ADC (axi_ad9361 channel 0, l_clk domain)
  (* X_INTERFACE_INFO = "xilinx.com:signal:clock:1.0 adc_clk CLK" *)
  (* X_INTERFACE_PARAMETER = "ASSOCIATED_RESET adc_rst" *)
  input  wire         adc_clk,
  (* X_INTERFACE_INFO = "xilinx.com:signal:reset:1.0 adc_rst RST" *)
  (* X_INTERFACE_PARAMETER = "POLARITY ACTIVE_HIGH" *)
  input  wire         adc_rst,
  input  wire         adc_valid,
  input  wire [15:0]  adc_data_i,
  input  wire [15:0]  adc_data_q,

  // to xfft S_AXIS_DATA / S_AXIS_CONFIG
  output wire [31:0]  m_axis_fft_tdata,
  output wire         m_axis_fft_tvalid,
  input  wire         m_axis_fft_tready,
  output wire         m_axis_fft_tlast,
  output wire [7:0]   m_axis_fftcfg_tdata,
  output reg          m_axis_fftcfg_tvalid = 1'b0,
  input  wire         m_axis_fftcfg_tready,
  // from xfft M_AXIS_DATA
  input  wire [63:0]  s_axis_fftout_tdata,
  input  wire [15:0]  s_axis_fftout_tuser,
  input  wire         s_axis_fftout_tvalid,
  output wire         s_axis_fftout_tready,
  input  wire         s_axis_fftout_tlast,
  input  wire         fft_event_tlast_unexpected,
  input  wire         fft_event_tlast_missing,
  input  wire         fft_event_data_in_channel_halt,
  (* X_INTERFACE_INFO = "xilinx.com:signal:reset:1.0 fft_aresetn RST" *)
  (* X_INTERFACE_PARAMETER = "POLARITY ACTIVE_LOW" *)
  output reg          fft_aresetn = 1'b0,

  // to axi_dmac (history ring, cyclic) and axi_dmac (selected bins)
  output wire [63:0]  m_axis_hist_tdata,
  output wire         m_axis_hist_tvalid,
  input  wire         m_axis_hist_tready,
  output wire         m_axis_hist_tlast,
  output wire [63:0]  m_axis_sel_tdata,
  output wire         m_axis_sel_tvalid,
  input  wire         m_axis_sel_tready,
  output wire         m_axis_sel_tlast
);

  localparam [31:0] VERSION = 32'h0001_0000;
  localparam [31:0] MAGIC   = 32'h4348_414E;   // "CHAN"

  wire clk = s_axi_aclk;
  reg  srst = 1'b1;                    // system reset, registered (sync, active high)
  always @(posedge clk) srst <= ~s_axi_aresetn;

  // ---------------------------------------------------------------------------
  // register file
  wire        up_wreq, up_rreq;
  wire [13:0] up_waddr, up_raddr;
  wire [31:0] up_wdata;
  reg         up_wack = 1'b0, up_rack = 1'b0;
  reg  [31:0] up_rdata = 32'd0;

  up_axi #(.AXI_ADDRESS_WIDTH(16)) i_up_axi (
    .up_rstn (s_axi_aresetn), .up_clk (clk),
    .up_axi_awvalid (s_axi_awvalid), .up_axi_awaddr (s_axi_awaddr),
    .up_axi_awready (s_axi_awready), .up_axi_wvalid (s_axi_wvalid),
    .up_axi_wdata (s_axi_wdata), .up_axi_wstrb (s_axi_wstrb),
    .up_axi_wready (s_axi_wready), .up_axi_bvalid (s_axi_bvalid),
    .up_axi_bresp (s_axi_bresp), .up_axi_bready (s_axi_bready),
    .up_axi_arvalid (s_axi_arvalid), .up_axi_araddr (s_axi_araddr),
    .up_axi_arready (s_axi_arready), .up_axi_rvalid (s_axi_rvalid),
    .up_axi_rresp (s_axi_rresp), .up_axi_rdata (s_axi_rdata),
    .up_axi_rready (s_axi_rready),
    .up_wreq (up_wreq), .up_waddr (up_waddr), .up_wdata (up_wdata), .up_wack (up_wack),
    .up_rreq (up_rreq), .up_raddr (up_raddr), .up_rdata (up_rdata), .up_rack (up_rack)
  );

  reg  [13:0]  raddr2 = 14'd0;       // read address, registered (see read pipeline)
  reg  [31:0]  scratch = 32'd0;
  reg          run = 1'b0, hist_en = 1'b0, sel_en = 1'b0, auto_fwd = 1'b0;
  reg  [3:0]   out_shift = 4'd7;
  reg  [15:0]  on_q8 = DEFAULT_ON_Q8, off_q8 = DEFAULT_OFF_Q8, bias_q8 = DEFAULT_BIAS_Q8;
  reg  [7:0]   hang = DEFAULT_HANG;
  reg          det_clear = 1'b0;     // CONTROL bit 4: self-clearing detector reset
  reg          freeze = 1'b0;        // SNAP_CTRL bit 0

  always @(posedge clk) begin
    if (srst) begin
      up_wack <= 1'b0;
      scratch <= 32'd0;
      run <= 1'b0; hist_en <= 1'b0; sel_en <= 1'b0; auto_fwd <= 1'b0;
      freeze <= 1'b0;
      out_shift <= 4'd7;
      on_q8 <= DEFAULT_ON_Q8; off_q8 <= DEFAULT_OFF_Q8; bias_q8 <= DEFAULT_BIAS_Q8;
      hang <= DEFAULT_HANG;
    end else begin
      up_wack <= up_wreq;
      det_clear <= up_wreq & (up_waddr == 14'h004) & up_wdata[4];
      if (dp_rst) freeze <= 1'b0;       // a stopped core never stays frozen
      if (up_wreq) begin
        case (up_waddr)
          14'h002: scratch <= up_wdata;
          14'h004: {auto_fwd, sel_en, hist_en, run} <= up_wdata[3:0];
          14'h006: out_shift <= up_wdata[3:0];
          14'h008: on_q8 <= up_wdata[15:0];
          14'h009: off_q8 <= up_wdata[15:0];
          14'h00A: bias_q8 <= up_wdata[15:0];
          14'h00B: hang <= up_wdata[7:0];
          14'h022: freeze <= up_wdata[0];
          default: ;
        endcase
      end
    end
  end

  // datapath soft reset while not running (also during system reset)
  reg dp_rst = 1'b1;
  always @(posedge clk) dp_rst <= srst | ~run;
  wire hist_fifo_empty, sel_fifo_empty;

  // xfft: aresetn low >= 2 cycles with the datapath; one config beat (forward)
  always @(posedge clk) begin
    fft_aresetn <= ~dp_rst;
    if (dp_rst) m_axis_fftcfg_tvalid <= 1'b0;
    else if (!fft_aresetn) m_axis_fftcfg_tvalid <= 1'b1;
    else if (m_axis_fftcfg_tready) m_axis_fftcfg_tvalid <= 1'b0;
  end
  assign m_axis_fftcfg_tdata = 8'h01;

  // ---------------------------------------------------------------------------
  // datapath
  wire        cdc_valid, cdc_ready;
  wire [15:0] cdc_i, cdc_q;
  wire [31:0] adc_overflows;

  chan_adc_cdc i_cdc (
    .adc_clk (adc_clk), .adc_rst (adc_rst), .adc_valid (adc_valid),
    .adc_data_i (adc_data_i), .adc_data_q (adc_data_q),
    .clk (clk), .run (run), .m_valid (cdc_valid), .m_i (cdc_i), .m_q (cdc_q),
    .m_ready (cdc_ready), .overflow_count (adc_overflows)
  );

  function [11:0] clip12(input [15:0] v);
    clip12 = ($signed(v) > 16'sd2047) ? 12'h7FF :
             ($signed(v) < -16'sd2048) ? 12'h800 : v[11:0];
  endfunction
  wire clipped = cdc_valid & cdc_ready &
                 (($signed(cdc_i) > 16'sd2047) | ($signed(cdc_i) < -16'sd2048) |
                  ($signed(cdc_q) > 16'sd2047) | ($signed(cdc_q) < -16'sd2048));

  wire [15:0] u_i, u_q;
  wire        frozen;
  wire [31:0] snap_frame;
  reg  [11:0] snap_i = 12'd0;
  wire [23:0] snap_in_data;
  reg  [9:0]  snap_addr = 10'd0;
  wire [31:0] snap_out_data;
  wire        pack_busy;
  wire        u_last;
  wire [31:0] fir_frames;
  chan_pfb_fir #(.COEF_FILE_PREFIX(COEF_FILE_PREFIX)) i_fir (
    .clk (clk), .rst (dp_rst),
    .s_valid (cdc_valid & ~dp_rst), .s_i (clip12(cdc_i)), .s_q (clip12(cdc_q)),
    .s_ready (cdc_ready),
    .m_valid (m_axis_fft_tvalid), .m_i (u_i), .m_q (u_q), .m_last (u_last),
    .m_ready (m_axis_fft_tready), .frames_issued (fir_frames),
    .freeze_req (freeze), .frozen (frozen), .snap_frame (snap_frame),
    .snap_i (snap_i), .snap_data (snap_in_data)
  );
  // xfft input: XN_RE [15:0], XN_IM [31:16]
  assign m_axis_fft_tdata = {u_q, u_i};
  assign m_axis_fft_tlast = u_last;

  wire        fb_we, fb_bank, fd_valid, fd_sat;
  wire [8:0]  fb_addr;
  wire [31:0] fb_data;
  wire [47:0] fd_frame;
  wire        pw_valid, pw_last;
  wire [8:0]  pw_k;
  wire [35:0] pw;
  wire [31:0] sat_count;

  chan_post i_post (
    .clk (clk), .rst (dp_rst),
    .s_valid (s_axis_fftout_tvalid), .s_data (s_axis_fftout_tdata),
    .s_user (s_axis_fftout_tuser), .s_last (s_axis_fftout_tlast),
    .s_ready (s_axis_fftout_tready), .out_shift (out_shift),
    .fb_we (fb_we), .fb_bank (fb_bank), .fb_addr (fb_addr), .fb_data (fb_data),
    .fd_valid (fd_valid), .fd_frame (fd_frame), .fd_sat (fd_sat),
    .pw_valid (pw_valid), .pw_k (pw_k), .pw (pw), .pw_last (pw_last),
    .sat_count (sat_count)
  );

  wire [8:0]   sel_query;
  wire         mask_bit, active_bit;
  wire [31:0]  mask_word, active_word;

  // FORWARD_MASK: written a word at a time over AXI (reset value: nothing selected
  // is guaranteed by RUN = 0 at reset; the mask itself has no reset)
  chan_bitmap i_mask (
    .clk (clk), .we_bit (1'b0), .bit_addr (9'd0), .bit_data (1'b0),
    .we_word (up_wreq & (up_waddr[13:4] == 10'h004)), .word_waddr (up_waddr[3:0]),
    .word_wdata (up_wdata),
    .rd_bit_addr (sel_query), .rd_bit (mask_bit),
    .rd_word_addr (raddr2[3:0]), .rd_word (mask_word)
  );
  reg  [8:0]   lvl_raddr = 9'd0, psd_raddr = 9'd0;
  wire [31:0]  lvl_rdata;
  wire [15:0]  psd_rdata;
  wire [31:0]  block_count, psd_count;

  chan_activity #(
    .BLOCK_LOG2(BLOCK_LOG2), .SUB_BLOCKS(SUB_BLOCKS), .PSD_LOG2(PSD_LOG2),
    .LOG_TABLE(LOG_TABLE)
  ) i_activity (
    .clk (clk), .rst (dp_rst | det_clear),
    .pw_valid (pw_valid), .pw_k (pw_k), .pw (pw), .pw_last (pw_last),
    .on_q8 (on_q8), .off_q8 (off_q8), .bias_q8 (bias_q8), .hang_blocks (hang),
    .act_bit_addr (sel_query), .act_bit (active_bit),
    .act_word_addr (raddr2[3:0]), .act_word (active_word),
    .lvl_raddr (lvl_raddr), .lvl_rdata (lvl_rdata),
    .psd_raddr (psd_raddr), .psd_rdata (psd_rdata),
    .block_count (block_count), .psd_count (psd_count)
  );

  wire [31:0] hist_frames, hist_drops, sel_frames, sel_drops, overruns;
  wire [47:0] hist_first;
  chan_pack i_pack (
    .clk (clk), .rst (dp_rst), .fifo_rst (srst),
    .fb_we (fb_we), .fb_bank (fb_bank), .fb_addr (fb_addr), .fb_data (fb_data),
    .fd_valid (fd_valid), .fd_frame (fd_frame), .fd_sat (fd_sat),
    .hist_en (hist_en), .sel_en (sel_en), .auto_forward (auto_fwd),
    .sel_query (sel_query), .mask_bit (mask_bit), .active_bit (active_bit),
    .adc_overflow_count (adc_overflows),
    .m_hist_valid (m_axis_hist_tvalid), .m_hist_data (m_axis_hist_tdata),
    .m_hist_ready (m_axis_hist_tready), .m_hist_last (m_axis_hist_tlast),
    .m_sel_valid (m_axis_sel_tvalid), .m_sel_data (m_axis_sel_tdata),
    .m_sel_ready (m_axis_sel_tready), .m_sel_last (m_axis_sel_tlast),
    .hist_frames (hist_frames), .hist_drops (hist_drops), .hist_first_frame (hist_first),
    .sel_frames (sel_frames), .sel_drops (sel_drops), .overruns (overruns),
    .busy_o (pack_busy), .hist_empty_o (hist_fifo_empty), .sel_empty_o (sel_fifo_empty),
    .snap_addr (snap_addr), .snap_data (snap_out_data)
  );

  // ---------------------------------------------------------------------------
  // status counters (control path)
  reg [63:0] samples = 64'd0;
  reg [31:0] clips = 32'd0;
  reg [31:0] fft_events = 32'd0;
  reg [47:0] frames_out = 48'd0;
  always @(posedge clk) begin
    if (dp_rst) begin
      samples <= 64'd0; clips <= 32'd0; fft_events <= 32'd0; frames_out <= 48'd0;
    end else begin
      if (cdc_valid & cdc_ready) samples <= samples + 64'd1;
      if (clipped) clips <= clips + 32'd1;
      if (fft_event_tlast_unexpected | fft_event_tlast_missing | fft_event_data_in_channel_halt)
        fft_events <= fft_events + 32'd1;
      if (fd_valid) frames_out <= frames_out + 48'd1;
    end
  end

  // reads: up_raddr (t) -> RAM address registers (end of t) -> data by t+3 (FIR
  // banks have latency 2) -> up_rdata/up_rack (t+4). up_axi holds up_raddr for
  // the whole transaction.
  wire snap_out_ready = frozen & ({16'd0, frames_out} > {16'd0, snap_frame}) & ~pack_busy;
  reg [31:0] hi_latch = 32'd0;
  reg        rreq1 = 1'b0, rreq2 = 1'b0, rreq3 = 1'b0;
  always @(posedge clk) begin
    lvl_raddr <= up_raddr[8:0];
    psd_raddr <= up_raddr[8:0];
    snap_i <= up_raddr[11:0];
    snap_addr <= {snap_frame[0], up_raddr[8:0]};
    raddr2 <= up_raddr;
    if (srst) begin
      rreq1 <= 1'b0; rreq2 <= 1'b0; rreq3 <= 1'b0; up_rack <= 1'b0; up_rdata <= 32'd0;
    end else begin
      rreq1 <= up_rreq;
      rreq2 <= rreq1;
      rreq3 <= rreq2;
      up_rack <= rreq3;
      if (rreq3) begin
        casez (raddr2)
          14'h000: up_rdata <= VERSION;
          14'h001: up_rdata <= ID;
          14'h002: up_rdata <= scratch;
          14'h003: up_rdata <= MAGIC;
          14'h004: up_rdata <= {28'd0, auto_fwd, sel_en, hist_en, run};
          14'h005: up_rdata <= {28'd0, pack_busy, sel_fifo_empty, hist_fifo_empty, ~dp_rst};
          14'h006: up_rdata <= {28'd0, out_shift};
          14'h007: up_rdata <= {BLOCK_LOG2[7:0], 8'd8, 16'd512};  // block_log2, taps, bins
          14'h00C: up_rdata <= {PSD_LOG2[7:0], 8'd4, SUB_BLOCKS[15:0]};  // psd_log2, sub-windows, U
          14'h008: up_rdata <= {16'd0, on_q8};
          14'h009: up_rdata <= {16'd0, off_q8};
          14'h00A: up_rdata <= {16'd0, bias_q8};
          14'h00B: up_rdata <= {24'd0, hang};
          14'h010: begin up_rdata <= samples[31:0]; hi_latch <= samples[63:32]; end
          14'h011: up_rdata <= hi_latch;
          14'h012: begin up_rdata <= frames_out[31:0]; hi_latch <= {16'd0, frames_out[47:32]}; end
          14'h013: up_rdata <= hi_latch;
          14'h014: up_rdata <= adc_overflows;
          14'h015: up_rdata <= clips;
          14'h016: up_rdata <= sat_count;
          14'h017: up_rdata <= hist_frames;
          14'h018: up_rdata <= hist_drops;
          14'h019: up_rdata <= sel_frames;
          14'h01A: up_rdata <= sel_drops;
          14'h01B: up_rdata <= overruns;
          14'h01C: up_rdata <= block_count;
          14'h01D: up_rdata <= psd_count;
          14'h01E: begin up_rdata <= hist_first[31:0]; hi_latch <= {16'd0, hist_first[47:32]}; end
          14'h01F: up_rdata <= hi_latch;
          14'h020: up_rdata <= fft_events;
          14'h021: up_rdata <= fir_frames;
          14'h022: up_rdata <= {29'd0, snap_out_ready, frozen, freeze};
          14'h023: up_rdata <= snap_frame;
          14'b00_011?_????_????: up_rdata <= snap_out_data;           // 0x600-0x7FF
          14'b01_????_????_????: up_rdata <= {{4{snap_in_data[23]}}, snap_in_data[23:12],
                                               {4{snap_in_data[11]}}, snap_in_data[11:0]};
          14'b00_0000_0100_????: up_rdata <= mask_word;
          14'b00_0000_0101_????: up_rdata <= active_word;
          14'b00_001?_????_????: up_rdata <= lvl_rdata;
          14'b00_010?_????_????: up_rdata <= {16'd0, psd_rdata};
          default: up_rdata <= 32'd0;
        endcase
      end
    end
  end

endmodule

`default_nettype wire
