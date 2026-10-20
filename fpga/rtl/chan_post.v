// FFT output stage: phase correction, output scaling, power word.
//
// Input: xfft m_axis_data (any order: XK_INDEX in tuser gives k), unscaled 26-bit
// in 32-bit fields.
// * Odd bins of odd frames are negated: a hop of M/2 rotates bin k by
//   exp(-j pi k f) = (-1)^(k f) (harris 2004; Vallance 2017), matching pfb.c.
// * Slice samples: round-half-up >> out_shift, saturate to signed 16 (RIQ1 I16/Q16).
// * Power word for PSD/activity: components rounded >> 6 and saturated to 18 bits,
//   then I^2 + Q^2 (two DSP48E1 multiplies). Matches channelizer.power_word().
//
// Never applies backpressure (s_ready = 1): every downstream consumer either keeps
// up by construction or drops whole frames and counts them.

`timescale 1ns/1ps
`default_nettype none

module chan_post (
  input  wire        clk,
  input  wire        rst,

  input  wire        s_valid,
  input  wire [63:0] s_data,
  input  wire [15:0] s_user,
  input  wire        s_last,
  output wire        s_ready,

  input  wire [3:0]  out_shift,

  // frame buffer write (natural bin order)
  output reg         fb_we = 1'b0,
  output reg         fb_bank = 1'b0,
  output reg  [8:0]  fb_addr = 9'd0,
  output reg  [31:0] fb_data = 32'd0,
  output reg         fd_valid = 1'b0,       // frame complete in fb_bank
  output reg  [47:0] fd_frame = 48'd0,
  output reg         fd_sat = 1'b0,

  // power words
  output reg         pw_valid = 1'b0,
  output reg  [8:0]  pw_k = 9'd0,
  output reg  [35:0] pw = 36'd0,
  output reg         pw_last = 1'b0,

  output reg  [31:0] sat_count = 32'd0
);

  assign s_ready = 1'b1;

  reg  [47:0] frame = 48'd0;

  // c1: capture
  reg               v1 = 1'b0;
  reg signed [27:0] yi1, yq1;
  reg        [8:0]  k1;
  reg               last1, neg1;
  reg        [47:0] f1;
  always @(posedge clk) begin
    yi1 <= s_data[27:0];
    yq1 <= s_data[59:32];
    k1 <= s_user[8:0];
    last1 <= s_last;
    neg1 <= s_user[0] & frame[0];
    f1 <= frame;
  end

  // c2: phase correction (the bound |y| < 2^23.2 makes negation overflow-free)
  reg               v2 = 1'b0;
  reg signed [27:0] yi2, yq2;
  reg        [8:0]  k2;
  reg               last2;
  reg        [47:0] f2;
  always @(posedge clk) begin
    yi2 <= neg1 ? -yi1 : yi1;
    yq2 <= neg1 ? -yq1 : yq1;
    k2 <= k1; last2 <= last1; f2 <= f1;
  end

  // c3: rounding shifts
  function signed [27:0] rshift(input signed [27:0] v, input [3:0] s);
    reg signed [28:0] t;
    begin
      t = (s == 4'd0) ? {v[27], v} : ({v[27], v} + (29'sd1 <<< (s - 4'd1)));
      rshift = t >>> s;
    end
  endfunction
  function [17:0] sat18(input signed [27:0] v);
    begin
      if (v > 28'sd131071)       sat18 = 18'h1FFFF;
      else if (v < -28'sd131072) sat18 = 18'h20000;
      else                       sat18 = v[17:0];
    end
  endfunction

  reg               v3 = 1'b0;
  reg signed [27:0] oi3, oq3;
  reg signed [17:0] ai3, aq3;
  reg        [8:0]  k3;
  reg               last3;
  reg        [47:0] f3;
  always @(posedge clk) begin
    oi3 <= rshift(yi2, out_shift);
    oq3 <= rshift(yq2, out_shift);
    ai3 <= sat18(rshift(yi2, 4'd6));    // = Config.psd_shift
    aq3 <= sat18(rshift(yq2, 4'd6));
    k3 <= k2; last3 <= last2; f3 <= f2;
  end

  // c4: 16-bit saturation -> frame buffer; power products (DSP MREG)
  wire sat_i = (oi3 > 28'sd32767) | (oi3 < -28'sd32768);
  wire sat_q = (oq3 > 28'sd32767) | (oq3 < -28'sd32768);
  wire [15:0] si = (oi3 > 28'sd32767) ? 16'h7FFF : (oi3 < -28'sd32768) ? 16'h8000 : oi3[15:0];
  wire [15:0] sq = (oq3 > 28'sd32767) ? 16'h7FFF : (oq3 < -28'sd32768) ? 16'h8000 : oq3[15:0];

  (* use_dsp = "yes" *) reg [35:0] pii4, pqq4;
  reg        [8:0]  k4;
  reg               last4;
  reg               v4 = 1'b0;
  reg               sat_acc = 1'b0;
  always @(posedge clk) begin
    pii4 <= ai3 * ai3;
    pqq4 <= aq3 * aq3;
    k4 <= k3; last4 <= last3;
    fb_addr <= k3;
    fb_data <= {sq, si};
    fb_bank <= f3[0];
    fd_frame <= f3;
  end

  // c5: power sum
  always @(posedge clk) begin
    pw <= pii4 + pqq4;
    pw_k <= k4;
    pw_last <= last4;
  end

  // control path
  always @(posedge clk) begin
    if (rst) begin
      frame <= 48'd0;
      v1 <= 1'b0; v2 <= 1'b0; v3 <= 1'b0; v4 <= 1'b0;
      fb_we <= 1'b0; fd_valid <= 1'b0; fd_sat <= 1'b0; pw_valid <= 1'b0;
      sat_acc <= 1'b0; sat_count <= 32'd0;
    end else begin
      if (s_valid & s_last) frame <= frame + 48'd1;
      v1 <= s_valid; v2 <= v1; v3 <= v2; v4 <= v3;
      fb_we <= v3;
      fd_valid <= v3 & last3;
      fd_sat <= sat_acc | sat_i | sat_q;
      if (v3) begin
        sat_acc <= last3 ? 1'b0 : (sat_acc | sat_i | sat_q);
        if (sat_i | sat_q) sat_count <= sat_count + 32'd1;
      end
      pw_valid <= v4;
    end
  end

endmodule

`default_nettype wire
