// Self-checking proof that axi_ad9361's rx correction chain is a bit-exact identity at
// the ADI Linux driver's probe defaults, so building with ADC_DCFILTER_DISABLE=1 and
// ADC_IQCORRECTION_DISABLE=1 changes no sample value.
//
// Chain per channel (as in axi_ad9361_rx_channel.v), after ad_datafmt (12-bit sign-extended
// to 16 bits, CR = 16):
//     ad_dcfilter (dcfilt_enb = 0, dcfilt_offset = 0) -> ad_iqcor (iqcor_enable = 1)
//   I channel: Q_OR_I_N = 0, coeff_1 = 16'h4000 (1.0 in Q1.14), coeff_2 = 0
//   Q channel: Q_OR_I_N = 1, coeff_1 = 0,        coeff_2 = 16'h4000
// The I and Q iqcor blocks are cross-connected (data_iq) exactly as in the RTL.
//
// Every valid input sample must come out unchanged, in order, at the same valid_out
// strobe stream. Checked in two modes: valid on every clock and valid randomly gated.
// A negative control (a non-identity coefficient) confirms the comparison is not vacuous.
//
// Run (scratch dir; needs LD_LIBRARY_PATH to include ~/amd/compat-lib for libncurses.so.5):
//   xvlog -sv tb_adc_corr_identity.sv ad_dcfilter.v ad_iqcor.v ad_mul.v glbl.v
//   xelab -L unisims_ver -L unimacro_ver tb_adc_corr_identity glbl -s sim ; xsim sim -runall

`timescale 1ns/1ps

module tb_adc_corr_identity;

  localparam int N_PER_MODE = 120000;     // valid samples per mode (>= 100k)

  logic clk = 0;
  always #5 clk = ~clk;

  // stimulus
  logic        valid = 0;
  logic [15:0] d_i = 0, d_q = 0;          // sign-extended 12-bit
  logic        dcfilt_enb = 0;
  logic [15:0] dcfilt_coeff = 16'h1234;   // arbitrary: must not matter while disabled
  logic [15:0] dcfilt_offset = 0;
  logic        iqcor_enable = 1;
  logic [15:0] i_c1 = 16'h4000, i_c2 = 16'h0000;
  logic [15:0] q_c1 = 16'h0000, q_c2 = 16'h4000;

  // device under test: two dcfilters, two iqcor
  wire        dci_v, dcq_v;
  wire [15:0] dci_d, dcq_d;
  wire        oi_v, oq_v;
  wire [15:0] oi_d, oq_d;

  ad_dcfilter #(.DISABLE(0)) u_dc_i (.clk(clk), .valid(valid), .data(d_i),
    .valid_out(dci_v), .data_out(dci_d),
    .dcfilt_enb(dcfilt_enb), .dcfilt_coeff(dcfilt_coeff), .dcfilt_offset(dcfilt_offset));
  ad_dcfilter #(.DISABLE(0)) u_dc_q (.clk(clk), .valid(valid), .data(d_q),
    .valid_out(dcq_v), .data_out(dcq_d),
    .dcfilt_enb(dcfilt_enb), .dcfilt_coeff(dcfilt_coeff), .dcfilt_offset(dcfilt_offset));

  ad_iqcor #(.Q_OR_I_N(0), .CR(16)) u_cor_i (.clk(clk), .valid(dci_v),
    .data_in(dci_d), .data_iq(dcq_d), .valid_out(oi_v), .data_out(oi_d),
    .iqcor_enable(iqcor_enable), .iqcor_coeff_1(i_c1), .iqcor_coeff_2(i_c2));
  ad_iqcor #(.Q_OR_I_N(1), .CR(16)) u_cor_q (.clk(clk), .valid(dcq_v),
    .data_in(dcq_d), .data_iq(dci_d), .valid_out(oq_v), .data_out(oq_d),
    .iqcor_enable(iqcor_enable), .iqcor_coeff_1(q_c1), .iqcor_coeff_2(q_c2));

  // reference queues
  logic [15:0] qi[$], qq[$];
  int n_chk = 0, n_err = 0, n_diff = 0;
  bit check_identity = 1;

  always @(posedge clk) begin
    if (oi_v !== oq_v) begin n_err++; $display("ERROR valid_out mismatch I/Q at %0t", $time); end
    if (oi_v === 1'b1 && qi.size() > 0) begin
      automatic logic [15:0] ei = qi.pop_front();
      automatic logic [15:0] eq = qq.pop_front();
      n_chk++;
      if (check_identity) begin
        if (oi_d !== ei || oq_d !== eq) begin
          n_err++;
          if (n_err < 10) $display("ERROR #%0d: in (%0d,%0d) out (%0d,%0d)", n_chk,
            $signed(ei), $signed(eq), $signed(oi_d), $signed(oq_d));
        end
      end else if (oi_d !== ei || oq_d !== eq) n_diff++;
    end else if (oi_v === 1'b1) begin
      n_err++; $display("ERROR output with empty reference queue");
    end
  end

  // measure latency: clocks between a lone valid input and valid_out
  int lat = -1;

  task automatic drive(input int n, input int gate_pct, input bit extremes);
    int sent = 0;
    while (sent < n) begin
      @(negedge clk);
      if ($urandom_range(99) >= gate_pct) begin
        valid = 0;
        d_i = $urandom; d_q = $urandom;     // junk while invalid: must not leak
      end else begin
        int ri, rq;
        ri = $urandom_range(4095) - 2048;
        rq = $urandom_range(4095) - 2048;
        if (extremes && sent % 997 == 0) begin ri = -2048; rq = 2047; end
        if (extremes && sent % 997 == 1) begin ri = 2047;  rq = -2048; end
        if (extremes && sent % 997 == 2) begin ri = -2048; rq = -2048; end
        if (extremes && sent % 997 == 3) begin ri = 2047;  rq = 2047; end
        valid = 1;
        d_i = 16'(ri); d_q = 16'(rq);
        qi.push_back(d_i); qq.push_back(d_q);
        sent++;
      end
    end
    @(negedge clk) valid = 0;
    repeat (20) @(negedge clk);
  endtask

  initial begin
    // pipeline latency of a single sample
    repeat (10) @(negedge clk);
    begin
      int t0;
      @(negedge clk);
      valid = 1; d_i = 16'sd1234; d_q = -16'sd77; qi.push_back(d_i); qq.push_back(d_q);
      t0 = $time;
      @(negedge clk) valid = 0;
      wait (oi_v === 1'b1);
      lat = ($time - t0) / 10;
      @(negedge clk);
    end
    $display("single-sample latency: %0d clocks (valid in -> valid_out)", lat);

    drive(N_PER_MODE, 100, 1);      // valid every clock
    drive(N_PER_MODE, 50, 1);       // valid ~50% of clocks
    drive(N_PER_MODE, 25, 1);       // sparse valid
    $display("identity: %0d samples checked, %0d errors", n_chk, n_err);

    // negative control: a half-scale coefficient must change the output
    check_identity = 0; n_chk = 0;
    i_c1 = 16'h2000;
    drive(2000, 100, 0);
    $display("negative control: %0d of %0d samples differ with coeff_1=0x2000", n_diff, n_chk);
    if (n_diff < n_chk / 2) begin n_err++; $display("ERROR: negative control did not change output"); end

    if (n_err == 0) $display("PASS tb_adc_corr_identity (identity over 3x%0d samples, I and Q)", N_PER_MODE);
    else            $display("FAIL tb_adc_corr_identity: %0d errors", n_err);
    $finish;
  end
endmodule
