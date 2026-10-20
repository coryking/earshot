// Per-bin power spectrum and activity detector (bit-exact to fpga/model/activity.py).
//
// For each coarse bin k, from the power word stream (natural order, one frame =
// 512 words):
//  * Welch-averaged periodogram over K = 2^BLOCK_LOG2 frames (Welch 1967), reported
//    as a Q8 log2 level L.
//  * Noise floor N by minimum statistics (Martin 2001): min of L over the current
//    sub-window and the last V sub-windows of U blocks, plus bias_q8.
//  * Decision with hysteresis and hang (CFAR-style: thresholds are relative to the
//    estimated floor, so Pfa does not depend on absolute gain). Open when
//    L > N + on_q8; stay open while L > N + off_q8, then for `hang` more blocks.
//  * Long PSD: linear sum over 2^PSD_LOG2 frames, reported as Q8 log2.
//
// State is in block RAM, read-modify-written once per bin per frame. A bin recurs
// every 512 words, so the 5-cycle read-to-write distance never aliases.

`timescale 1ns/1ps
`default_nettype none

module chan_activity #(
  parameter BLOCK_LOG2 = 8,
  parameter SUB_BLOCKS = 128,
  parameter SUB_WINDOWS = 4,           // fixed at 4 by the 64-bit minima word
  parameter PSD_LOG2 = 13,
  parameter LOG_TABLE = "chan_log2_table.mem"
) (
  input  wire         clk,
  input  wire         rst,

  input  wire         pw_valid,
  input  wire [8:0]   pw_k,
  input  wire [35:0]  pw,
  input  wire         pw_last,

  input  wire [15:0]  on_q8,
  input  wire [15:0]  off_q8,
  input  wire [15:0]  bias_q8,
  input  wire [7:0]   hang_blocks,

  input  wire [8:0]   act_bit_addr,       // asynchronous bit read (packetizer)
  output wire         act_bit,
  input  wire [3:0]   act_word_addr,      // asynchronous word read (registers)
  output wire [31:0]  act_word,
  input  wire [8:0]   lvl_raddr,
  output reg  [31:0]  lvl_rdata = 32'd0,  // {floor, level}, latency 1
  input  wire [8:0]   psd_raddr,
  output reg  [15:0]  psd_rdata = 16'd0,  // latency 1
  output reg  [31:0]  block_count = 32'd0,
  output reg  [31:0]  psd_count = 32'd0
);

  localparam [15:0] EMPTY = 16'hFFFF;

  // ---------------------------------------------------------------------------
  // frame position counters (control)
  reg [BLOCK_LOG2-1:0] fib = 0;           // frame in block
  reg [7:0]            bis = 0;           // block in sub-window
  reg [PSD_LOG2-1:0]   pfc = 0;           // frame in long PSD
  wire blk_end0 = (fib == {BLOCK_LOG2{1'b1}});
  wire sub_end0 = blk_end0 & (bis == SUB_BLOCKS - 1);
  wire psd_end0 = (pfc == {PSD_LOG2{1'b1}});

  always @(posedge clk) begin
    if (rst) begin
      fib <= 0; bis <= 0; pfc <= 0;
    end else if (pw_valid & pw_last) begin
      fib <= fib + 1'b1;
      pfc <= pfc + 1'b1;
      if (blk_end0) bis <= sub_end0 ? 8'd0 : bis + 8'd1;
    end
  end

  // ---------------------------------------------------------------------------
  // state RAMs (simple dual port, read latency 1)
  //  A: {hang[7:0], active, sub_min[15:0], acc[42:0]} = 68 bits
  //  B: {min3, min2, min1, min0} (min0 newest)       = 64 bits
  //  C: psd_acc[47:0]
  reg  [67:0] ram_a [0:511];
  reg  [63:0] ram_b [0:511];
  reg  [47:0] ram_c [0:511];
  reg  [67:0] a1;
  reg  [63:0] b1;
  reg  [47:0] c1;

  reg         we_w;
  reg  [8:0]  k_w;
  reg  [67:0] a_w;
  reg  [63:0] b_w;
  reg  [47:0] c_w;

  // init so simulation and hardware start from "empty" minima and zero sums
  integer i;
  initial begin
    for (i = 0; i < 512; i = i + 1) begin
      ram_a[i] = {8'd0, 1'b0, EMPTY, 43'd0};
      ram_b[i] = {4{EMPTY}};
      ram_c[i] = 48'd0;
    end
  end

  // After any reset the state RAMs are swept back to their initial contents (512
  // cycles). The first frame cannot arrive sooner: the PFB needs 4096 samples.
  reg        clearing = 1'b1;
  reg  [8:0] clr_addr = 9'd0;
  always @(posedge clk) begin
    if (rst) begin
      clearing <= 1'b1;
      clr_addr <= 9'd0;
    end else if (clearing) begin
      clr_addr <= clr_addr + 9'd1;
      if (clr_addr == 9'd511) clearing <= 1'b0;
    end
  end

  always @(posedge clk) begin
    a1 <= ram_a[pw_k];
    b1 <= ram_b[pw_k];
    c1 <= ram_c[pw_k];
    if (clearing) begin
      ram_a[clr_addr] <= {8'd0, 1'b0, EMPTY, 43'd0};
      ram_b[clr_addr] <= {4{EMPTY}};
      ram_c[clr_addr] <= 48'd0;
    end else if (we_w) begin
      ram_a[k_w] <= a_w;
      ram_b[k_w] <= b_w;
      ram_c[k_w] <= c_w;
    end
  end

  // stage 1: side information aligned with RAM output
  reg        v1 = 1'b0, blk1, sub1, psd1, last1;
  reg [8:0]  k1;
  reg [35:0] p1;
  always @(posedge clk) begin
    k1 <= pw_k; p1 <= pw; blk1 <= blk_end0; sub1 <= sub_end0; psd1 <= psd_end0;
    last1 <= pw_last;
  end

  // stage 2: accumulate
  reg        v2 = 1'b0, blk2, sub2, psd2, last2;
  reg [8:0]  k2;
  reg [42:0] acc2;
  reg [47:0] pacc2;
  reg [15:0] smin2;
  reg        act2;
  reg [7:0]  hang2;
  reg [63:0] mins2;
  always @(posedge clk) begin
    acc2  <= a1[42:0] + {7'd0, p1};
    pacc2 <= c1 + {12'd0, p1};
    smin2 <= a1[58:43];
    act2  <= a1[59];
    hang2 <= a1[67:60];
    mins2 <= b1;
    k2 <= k1; blk2 <= blk1; sub2 <= sub1; psd2 <= psd1; last2 <= last1;
  end

  // stages 3-4: log2 (3-cycle pipeline) of both sums; side information waits in
  // stages 3, 3b and 4
  wire [15:0] lvl4, plv4;
  chan_log2 #(.W(48), .TABLE(LOG_TABLE)) i_log_lvl (
    .clk(clk), .v({5'd0, acc2}), .l(lvl4));
  chan_log2 #(.W(48), .TABLE(LOG_TABLE)) i_log_psd (
    .clk(clk), .v(pacc2), .l(plv4));

  reg        v3 = 1'b0, blk3, sub3, psd3, last3, act3;
  reg [8:0]  k3;
  reg [42:0] acc3;
  reg [47:0] pacc3;
  reg [15:0] smin3;
  reg [7:0]  hang3;
  reg [63:0] mins3;
  reg [15:0] mmin3;
  always @(posedge clk) begin
    k3 <= k2; blk3 <= blk2; sub3 <= sub2; psd3 <= psd2; last3 <= last2;
    acc3 <= acc2; pacc3 <= pacc2; smin3 <= smin2; act3 <= act2; hang3 <= hang2; mins3 <= mins2;
    mmin3 <= min16(min16(mins2[15:0], mins2[31:16]), min16(mins2[47:32], mins2[63:48]));
  end

  reg        v3b = 1'b0, blk3b, sub3b, psd3b, last3b, act3b;
  reg [8:0]  k3b;
  reg [42:0] acc3b;
  reg [47:0] pacc3b;
  reg [15:0] smin3b, mmin3b;
  reg [7:0]  hang3b;
  reg [63:0] mins3b;
  always @(posedge clk) begin
    k3b <= k3; blk3b <= blk3; sub3b <= sub3; psd3b <= psd3; last3b <= last3;
    acc3b <= acc3; pacc3b <= pacc3; smin3b <= smin3; act3b <= act3; hang3b <= hang3;
    mins3b <= mins3; mmin3b <= mmin3;
  end

  reg        v4 = 1'b0, blk4, sub4, psd4, last4, act4;
  reg [8:0]  k4;
  reg [42:0] acc4;
  reg [47:0] pacc4;
  reg [15:0] smin4, mmin4;
  reg [7:0]  hang4;
  reg [63:0] mins4;
  always @(posedge clk) begin
    k4 <= k3b; blk4 <= blk3b; sub4 <= sub3b; psd4 <= psd3b; last4 <= last3b;
    acc4 <= acc3b; pacc4 <= pacc3b; smin4 <= smin3b; act4 <= act3b; hang4 <= hang3b;
    mins4 <= mins3b; mmin4 <= mmin3b;
  end

  function [15:0] min16(input [15:0] x, input [15:0] y);
    min16 = (x < y) ? x : y;
  endfunction

  // stage 5: floor (min statistics + bias) and thresholds, registered
  wire [15:0] smin_new = min16(smin4, lvl4);
  wire [15:0] floor_raw = min16(smin_new, mmin4);
  wire [16:0] floor_b = {1'b0, floor_raw} + {1'b0, bias_q8};
  wire [15:0] floor_v = floor_b[16] ? EMPTY : floor_b[15:0];

  reg        v5 = 1'b0, blk5, sub5, psd5, last5, act5;
  reg [8:0]  k5;
  reg [15:0] lvl5, plv5, floor5, smin5, sminold5;
  reg [17:0] thr_on5, thr_off5;
  reg [42:0] acc5;
  reg [47:0] pacc5;
  reg [7:0]  hang5;
  reg [63:0] mins5;
  always @(posedge clk) begin
    k5 <= k4; blk5 <= blk4; sub5 <= sub4; psd5 <= psd4; last5 <= last4; act5 <= act4;
    lvl5 <= lvl4; plv5 <= plv4; floor5 <= floor_v; smin5 <= smin_new; sminold5 <= smin4;
    thr_on5 <= {2'b0, floor_v} + {2'b0, on_q8};
    thr_off5 <= {2'b0, floor_v} + {2'b0, off_q8};
    acc5 <= acc4; pacc5 <= pacc4; hang5 <= hang4; mins5 <= mins4;
  end

  // stage 6: CFAR decision with hysteresis and hang; write-back
  wire        above_on  = {2'b0, lvl5} > thr_on5;
  wire        above_off = {2'b0, lvl5} > thr_off5;
  wire        keep   = act5 & above_off;
  wire        opened = ~act5 & above_on;
  wire        hold   = act5 & ~above_off & (hang5 != 8'd0);
  wire [7:0]  hang_n = (opened | keep) ? hang_blocks : (hold ? hang5 - 8'd1 : 8'd0);
  wire        act_n  = opened | keep | hold;

  always @(posedge clk) begin
    k_w <= k5;
    if (blk5) begin
      a_w <= {hang_n, act_n, (sub5 ? EMPTY : smin5), 43'd0};
      b_w <= sub5 ? {mins5[47:0], smin5} : mins5;
    end else begin
      a_w <= {hang5, act5, sminold5, acc5};
      b_w <= mins5;
    end
    c_w <= psd5 ? 48'd0 : pacc5;
  end

  // activity bitmap: written per bin at block end (cleared during the RAM sweep)
  chan_bitmap i_active (
    .clk (clk),
    .we_bit (clearing | (v5 & blk5)), .bit_addr (clearing ? clr_addr : k5),
    .bit_data (clearing ? 1'b0 : act_n),
    .we_word (1'b0), .word_waddr (4'd0), .word_wdata (32'd0),
    .rd_bit_addr (act_bit_addr), .rd_bit (act_bit),
    .rd_word_addr (act_word_addr), .rd_word (act_word)
  );

  // control: valid pipeline, readout RAMs, counters
  reg [31:0] lvl_ram [0:511];
  reg [15:0] psd_ram [0:511];
  always @(posedge clk) begin
    lvl_rdata <= lvl_ram[lvl_raddr];
    psd_rdata <= psd_ram[psd_raddr];
    if (v5 & blk5) lvl_ram[k5] <= {floor5, lvl5};
    if (v5 & psd5) psd_ram[k5] <= plv5;
  end

  always @(posedge clk) begin
    if (rst) begin
      v1 <= 1'b0; v2 <= 1'b0; v3 <= 1'b0; v3b <= 1'b0; v4 <= 1'b0; v5 <= 1'b0; we_w <= 1'b0;
      block_count <= 32'd0; psd_count <= 32'd0;
    end else begin
      v1 <= pw_valid & ~clearing; v2 <= v1; v3 <= v2; v3b <= v3; v4 <= v3b; v5 <= v4;
      we_w <= v5;
      if (v5 & blk5 & last5) block_count <= block_count + 32'd1;
      if (v5 & psd5 & last5) psd_count <= psd_count + 32'd1;
    end
  end

endmodule

// Q8 log2 of an unsigned integer: leading-one position * 256 + table[8 mantissa
// bits below the leading one]. log2(0) = 0. Three-cycle latency: priority encode,
// normalize, table look-up (one register per step for 100 MHz on a -1 part).
module chan_log2 #(
  parameter W = 48,
  parameter TABLE = "chan_log2_table.mem"
) (
  input  wire         clk,
  input  wire [W-1:0] v,
  output reg  [15:0]  l = 16'd0
);
  reg [7:0] tbl [0:255];
  initial $readmemh(TABLE, tbl);

  integer n;
  reg [5:0] e;
  always @(*) begin
    e = 6'd0;
    for (n = 0; n < W; n = n + 1) if (v[n]) e = n[5:0];
  end

  // mantissa = the 8 bits below the leading one: vx[e +: 8] with vx = {v, 8'b0},
  // an 8-bit variable part-select (8 x 48:1) rather than a full barrel shift
  reg [W+7:0] vx1;
  reg [5:0]   e1, e2;
  reg         nz1, nz2;
  reg [7:0]   m2;
  always @(posedge clk) begin
    vx1 <= {v, 8'd0};  e1 <= e;  nz1 <= |v;
    m2 <= vx1[e1 +: 8];  e2 <= e1;  nz2 <= nz1;
    l <= nz2 ? {2'b00, e2, 8'd0} + {8'd0, tbl[m2]} : 16'd0;
  end
endmodule

// 512-bit map in distributed RAM: 32 columns of 16 x 1. Bit writes (one bin) or
// word writes (32 bins), and two asynchronous read ports (one bit, one word).
module chan_bitmap (
  input  wire        clk,
  input  wire        we_bit,
  input  wire [8:0]  bit_addr,
  input  wire        bit_data,
  input  wire        we_word,
  input  wire [3:0]  word_waddr,
  input  wire [31:0] word_wdata,
  input  wire [8:0]  rd_bit_addr,
  output wire        rd_bit,
  input  wire [3:0]  rd_word_addr,
  output wire [31:0] rd_word
);
  wire [31:0] row;
  genvar c;
  generate
    for (c = 0; c < 32; c = c + 1) begin : g_col
      (* ram_style = "distributed" *) reg col [0:15];
      integer r;
      initial for (r = 0; r < 16; r = r + 1) col[r] = 1'b0;
      wire we = (we_bit & (bit_addr[4:0] == c)) | we_word;
      wire [3:0] wa = we_word ? word_waddr : bit_addr[8:5];
      wire wd = we_word ? word_wdata[c] : bit_data;
      always @(posedge clk) if (we) col[wa] <= wd;
      assign rd_word[c] = col[rd_word_addr];
      assign row[c] = col[rd_bit_addr[8:5]];
    end
  endgenerate
  assign rd_bit = row[rd_bit_addr[4:0]];
endmodule

`default_nettype wire
