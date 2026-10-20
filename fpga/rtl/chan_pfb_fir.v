// Polyphase partition of the 4096-tap prototype: 512 branches x 8 taps, hop 256.
//
// harris (2004, ch. 6) / Crochiere & Rabiner (1983): the branch sums
//   u[f][j] = sum_t h[t*512 + j] * x[256 f + t*512 + j],   j = 0..511
// feed a 512-point FFT. This is exactly the alignment of offload/arm/pfb.c and of
// fpga/model/channelizer.py (fir_fixed).
//
// Storage: the most recent 4096 input samples live in 8 banks of 512 (bank =
// address[11:9]). For a given j all 8 taps sit at the same offset in 8 different
// banks, so one cycle reads all of them; the coefficient for bank b is the tap
// t = (b - bank0) mod 8, selected by a rotation of the 8 coefficient ROM outputs.
//
// Throughput: one branch sum per clock (8 complex x real MACs = 16 DSP48E1), so a
// frame takes 512 clocks of the 1000 available per hop at 25.6 Msps / 100 MHz.
//
// Write protection: sample n overwrites sample n-4096. It may only do so once the
// engine has issued the read of that address for the oldest unfinished frame, and
// at most one hop (256 samples) ahead of it, so writes are allowed while
// d < 4096 + min(j_issued, 256), with d = samples written past the current frame's
// first sample. The FIFO in front absorbs the brief stalls.
//
// Output: AXI4-Stream-like (valid/ready/last) of 16-bit branch sums, last on j=511.
// A small FIFO after the arithmetic pipeline lets the FFT apply backpressure; the
// issue stage only starts a read when the FIFO has room for everything in flight.

`timescale 1ns/1ps
`default_nettype none

module chan_pfb_fir #(
  parameter COEF_FILE_PREFIX = "pfb_coef_p"   // pfb_coef_p0.mem .. p3.mem
) (
  input  wire        clk,
  input  wire        rst,          // synchronous, control state only

  input  wire        s_valid,
  input  wire [11:0] s_i,
  input  wire [11:0] s_q,
  output wire        s_ready,

  output wire        m_valid,
  output wire [15:0] m_i,
  output wire [15:0] m_q,
  output wire        m_last,
  input  wire        m_ready,

  output reg  [31:0] frames_issued = 32'd0,

  // Live self-test snapshot: freeze_req stops sample writes at the next frame
  // boundary, lets that frame's reads issue, then holds its exact 4096-sample
  // window in the banks for readback (snap_i = 0 is the oldest sample).
  input  wire        freeze_req,
  output wire        frozen,
  output reg  [31:0] snap_frame = 32'd0,
  input  wire [11:0] snap_i,
  output wire [23:0] snap_data         // {Q, I}, latency 2 from snap_i
);

  localparam FIR_SHIFT = 14;     // = fpga/model/channelizer.py Config.fir_shift
  localparam PIPE = 9;             // issue (cycle c) -> FIFO write (cycle c+9)
  localparam FIFO_AW = 5;          // 32-entry output FIFO

  // ---------------------------------------------------------------------------
  // write side
  reg  [11:0] waddr = 12'd0;       // n mod 4096
  reg  [12:0] d = 13'd0;           // samples written relative to current frame start
  reg  [9:0]  j_issued = 10'd0;    // reads issued for the current frame (0..511)

  // snapshot state: 0 run, 1 stop writing at the frame boundary, 2 issuing the
  // captured frame (no writes), 3 frozen (bank read port belongs to readback)
  reg  [1:0]  snap_st = 2'd0;
  reg  [11:0] snap_base = 12'd0;
  wire frame_ready = (d >= 13'd4096) & (snap_st != 2'd3);
  // A write may replace sample n-4096 only after its read issued, and never more
  // than one hop ahead: samples beyond 256 into the current frame belong to the
  // next frame's first tap, which has not been read yet.
  wire [12:0] wlim = (j_issued > 10'd256) ? 13'd4352 : 13'd4096 + {3'd0, j_issued};
  wire write_ok    = (d < wlim) &
                     ((snap_st == 2'd0) | ((snap_st == 2'd1) & (d < 13'd4096)));
  wire do_write    = s_valid & write_ok;
  assign s_ready   = write_ok;   // while frozen the CDC FIFO fills and counts overflows
  assign frozen    = (snap_st == 2'd3);

  // ---------------------------------------------------------------------------
  // issue side
  reg  [11:0] fbase = 12'd0;       // 256 * frame mod 4096
  reg  [FIFO_AW:0] fifo_count = 0;
  reg  [PIPE-1:0]  vld = {PIPE{1'b0}};
  reg  [4:0]  inflight = 5'd0;     // issued reads not yet written to the FIFO
  wire        fifo_room = ({1'b0, fifo_count} + {2'b0, inflight}) < (1 << FIFO_AW);
  wire        issue = frame_ready & fifo_room;
  wire        issue_last = issue & (j_issued == 10'd511);
  wire [11:0] issue_addr = fbase + {3'd0, j_issued[8:0]};
  wire [11:0] snap_abs = snap_base + snap_i;
  wire [11:0] raddr_abs = frozen ? snap_abs : issue_addr;


  always @(posedge clk) begin
    if (rst) begin
      waddr <= 12'd0;
      d <= 13'd0;
      j_issued <= 10'd0;
      fbase <= 12'd0;
      frames_issued <= 32'd0;
      snap_st <= 2'd0;
    end else begin
      case (snap_st)
        2'd0: if (freeze_req) snap_st <= 2'd1;
        2'd1: if (!freeze_req) snap_st <= 2'd0;
              else if (d == 13'd4096 && j_issued == 10'd0) begin
                snap_st <= 2'd2;
                snap_base <= fbase;
                snap_frame <= frames_issued;
              end
        2'd2: if (issue_last) snap_st <= 2'd3;
        2'd3: if (!freeze_req) snap_st <= 2'd0;
      endcase
      if (do_write) waddr <= waddr + 12'd1;
      // d changes by +1 per write and -256 when a frame's last read issues
      d <= d + {12'd0, do_write} - (issue_last ? 13'd256 : 13'd0);
      if (issue_last) begin
        j_issued <= 10'd0;
        fbase <= fbase + 12'd256;
        frames_issued <= frames_issued + 32'd1;
      end else if (issue) begin
        j_issued <= j_issued + 10'd1;
      end
    end
  end

  // ---------------------------------------------------------------------------
  // sample banks: 8 x (512 x 24) simple dual port, one write port shared by address
  reg  [2:0]  bank0_p0;
  reg         last_p0;
  wire [23:0] bank_dout [0:7];

  reg  [2:0]  snap_bank_p0, snap_bank_p1;
  always @(posedge clk) begin
    bank0_p0  <= raddr_abs[11:9];
    last_p0   <= issue_last;
    snap_bank_p0 <= snap_abs[11:9];
    snap_bank_p1 <= snap_bank_p0;
  end
  assign snap_data = bank_dout[snap_bank_p1];

  genvar b;
  generate
    for (b = 0; b < 8; b = b + 1) begin : g_bank
      chan_sdp_ram #(.AW(9), .DW(24)) i_bank (
        .clk   (clk),
        .we    (do_write & (waddr[11:9] == b)),
        .waddr (waddr[8:0]),
        .wdata ({s_q, s_i}),
        .raddr (raddr_abs[8:0]),
        .rdata (bank_dout[b])
      );
    end
  endgenerate

  // coefficient ROMs: 4 dual-port ROMs, each holding taps 2m (addr 0..511) and
  // 2m+1 (addr 512..1023)
  wire [17:0] coef [0:7];
  genvar m;
  generate
    for (m = 0; m < 4; m = m + 1) begin : g_coef
      localparam [7:0] DIGIT = 8'd48 + m;
      chan_rom2 #(
        .AW(10), .DW(18),
        .INIT_FILE({COEF_FILE_PREFIX, DIGIT, ".mem"})
      ) i_rom (
        .clk    (clk),
        .addr_a ({1'b0, j_issued[8:0]}),
        .addr_b ({1'b1, j_issued[8:0]}),
        .data_a (coef[2*m]),
        .data_b (coef[2*m+1])
      );
    end
  endgenerate

  // Timing (cycle in which a value is valid): issue at c; *_p0 at c+1; *_p1 and the
  // RAM/ROM outputs (address latched at the end of c, output register) at c+2.
  reg  [2:0] bank0_p1;
  reg        last_p1;
  always @(posedge clk) begin
    bank0_p1 <= bank0_p0;
    last_p1  <= last_p0;
  end

  // p3: coefficient rotation, data register (DSP A/B input registers)
  reg signed [17:0] c_p3 [0:7];
  reg signed [11:0] xi_p3 [0:7];
  reg signed [11:0] xq_p3 [0:7];
  reg               last_p3;
  integer k;
  always @(posedge clk) begin
    for (k = 0; k < 8; k = k + 1) begin
      c_p3[k]  <= coef[(k - bank0_p1) & 7];
      xi_p3[k] <= bank_dout[k][11:0];
      xq_p3[k] <= bank_dout[k][23:12];
    end
    last_p3 <= last_p1;
  end

  // p4: multiply (MREG), p5: product register (PREG)
  (* use_dsp = "yes" *) reg signed [29:0] pi_p4 [0:7];
  (* use_dsp = "yes" *) reg signed [29:0] pq_p4 [0:7];
  reg signed [29:0] pi_p5 [0:7];
  reg signed [29:0] pq_p5 [0:7];
  reg               last_p4, last_p5;
  always @(posedge clk) begin
    for (k = 0; k < 8; k = k + 1) begin
      pi_p4[k] <= xi_p3[k] * c_p3[k];
      pq_p4[k] <= xq_p3[k] * c_p3[k];
      pi_p5[k] <= pi_p4[k];
      pq_p5[k] <= pq_p4[k];
    end
    last_p4 <= last_p3;
    last_p5 <= last_p4;
  end

  // p6..p8: adder tree
  reg signed [31:0] si_p6 [0:3];
  reg signed [31:0] sq_p6 [0:3];
  reg signed [32:0] si_p7 [0:1];
  reg signed [32:0] sq_p7 [0:1];
  reg signed [33:0] si_p8, sq_p8;
  reg               last_p6, last_p7, last_p8;
  always @(posedge clk) begin
    for (k = 0; k < 4; k = k + 1) begin
      si_p6[k] <= pi_p5[2*k] + pi_p5[2*k+1];
      sq_p6[k] <= pq_p5[2*k] + pq_p5[2*k+1];
    end
    for (k = 0; k < 2; k = k + 1) begin
      si_p7[k] <= si_p6[2*k] + si_p6[2*k+1];
      sq_p7[k] <= sq_p6[2*k] + sq_p6[2*k+1];
    end
    si_p8 <= si_p7[0] + si_p7[1];
    sq_p8 <= sq_p7[0] + sq_p7[1];
    last_p6 <= last_p5; last_p7 <= last_p6; last_p8 <= last_p7;
  end

  // p9: round half up, >> FIR_SHIFT, saturate to 16 bits (xfft input width)
  function [15:0] round_sat(input signed [33:0] v);
    reg signed [33:0] r;
    begin
      r = (v + (34'sd1 <<< (FIR_SHIFT - 1))) >>> FIR_SHIFT;
      if (r > 34'sd32767)       round_sat = 16'h7FFF;
      else if (r < -34'sd32768) round_sat = 16'h8000;
      else                      round_sat = r[15:0];
    end
  endfunction

  reg [15:0] ui_p9, uq_p9;
  reg        last_p9;
  always @(posedge clk) begin
    ui_p9 <= round_sat(si_p8);
    uq_p9 <= round_sat(sq_p8);
    last_p9 <= last_p8;
  end

  // valid pipeline (control: reset)
  wire out_we = vld[PIPE-1];
  always @(posedge clk) begin
    if (rst) begin
      vld <= {PIPE{1'b0}};
      inflight <= 5'd0;
    end else begin
      vld <= {vld[PIPE-2:0], issue};
      inflight <= inflight + {4'd0, issue} - {4'd0, out_we};
    end
  end

  // ---------------------------------------------------------------------------
  // output FIFO (distributed RAM)
  reg [32:0] fifo_mem [0:(1<<FIFO_AW)-1];
  reg [FIFO_AW-1:0] wp = 0, rp = 0;
  wire fifo_rd = m_valid & m_ready;
  always @(posedge clk) begin
    if (out_we) fifo_mem[wp] <= {last_p9, uq_p9, ui_p9};
  end
  always @(posedge clk) begin
    if (rst) begin
      wp <= 0; rp <= 0; fifo_count <= 0;
    end else begin
      if (out_we) wp <= wp + 1'b1;
      if (fifo_rd) rp <= rp + 1'b1;
      fifo_count <= fifo_count + out_we - fifo_rd;
    end
  end
  assign m_valid = (fifo_count != 0);
  assign {m_last, m_q, m_i} = fifo_mem[rp];

endmodule

// Simple dual-port RAM, synchronous read with output register (BRAM inference,
// UG901 "Simple Dual-Port Block RAM"). Read latency 2.
module chan_sdp_ram #(
  parameter AW = 9,
  parameter DW = 24
) (
  input  wire          clk,
  input  wire          we,
  input  wire [AW-1:0] waddr,
  input  wire [DW-1:0] wdata,
  input  wire [AW-1:0] raddr,
  output reg  [DW-1:0] rdata
);
  (* ram_style = "block" *) reg [DW-1:0] mem [0:(1<<AW)-1];
  reg [DW-1:0] q;
  always @(posedge clk) begin
    if (we) mem[waddr] <= wdata;
    q <= mem[raddr];
    rdata <= q;
  end
endmodule

// Dual-port ROM initialized from a hex file. Read latency 2.
module chan_rom2 #(
  parameter AW = 10,
  parameter DW = 18,
  parameter INIT_FILE = "rom.mem"
) (
  input  wire          clk,
  input  wire [AW-1:0] addr_a,
  input  wire [AW-1:0] addr_b,
  output reg  [DW-1:0] data_a,
  output reg  [DW-1:0] data_b
);
  (* rom_style = "block" *) reg [DW-1:0] mem [0:(1<<AW)-1];
  reg [DW-1:0] qa, qb;
  initial $readmemh(INIT_FILE, mem);
  always @(posedge clk) begin
    qa <= mem[addr_a];
    qb <= mem[addr_b];
    data_a <= qa;
    data_b <= qb;
  end
endmodule

`default_nettype wire
