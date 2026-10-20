// Vector-in / vector-out harness for the rx correction chain, used to confirm the Python
// emulation in fpga/model/corrections.py against the real RTL (ad_dcfilter -> ad_iqcor).
// It checks nothing itself; corrections.py `verify-rtl` compares the files it writes.
//
//   plusargs: +in=<file of "i q" decimal pairs>  +out=<file>  +dcenb=0|1  +dccoeff=<n>
//             +ic1= +ic2= +qc1= +qc2=   (signed 16-bit iqcor coefficients, Q1.14)
//             +ratio=<clocks per sample, default 1>
// Output line per valid iqcor output: "<dc_i> <dc_q> <out_i> <out_q>" (dcfilter outputs are
// matched by order to the iqcor outputs; the pipeline keeps them aligned).

`timescale 1ns/1ps

module tb_adc_corr_vec;
  logic clk = 0;
  always #5 clk = ~clk;

  logic        valid = 0;
  logic [15:0] d_i = 0, d_q = 0;
  logic        dcfilt_enb;
  logic [15:0] dcfilt_coeff;
  logic [15:0] i_c1, i_c2, q_c1, q_c2;

  wire        dci_v, dcq_v, oi_v, oq_v;
  wire [15:0] dci_d, dcq_d, oi_d, oq_d;

  ad_dcfilter #(.DISABLE(0)) u_dc_i (.clk(clk), .valid(valid), .data(d_i),
    .valid_out(dci_v), .data_out(dci_d),
    .dcfilt_enb(dcfilt_enb), .dcfilt_coeff(dcfilt_coeff), .dcfilt_offset(16'd0));
  ad_dcfilter #(.DISABLE(0)) u_dc_q (.clk(clk), .valid(valid), .data(d_q),
    .valid_out(dcq_v), .data_out(dcq_d),
    .dcfilt_enb(dcfilt_enb), .dcfilt_coeff(dcfilt_coeff), .dcfilt_offset(16'd0));
  ad_iqcor #(.Q_OR_I_N(0), .CR(16)) u_cor_i (.clk(clk), .valid(dci_v),
    .data_in(dci_d), .data_iq(dcq_d), .valid_out(oi_v), .data_out(oi_d),
    .iqcor_enable(1'b1), .iqcor_coeff_1(i_c1), .iqcor_coeff_2(i_c2));
  ad_iqcor #(.Q_OR_I_N(1), .CR(16)) u_cor_q (.clk(clk), .valid(dcq_v),
    .data_in(dcq_d), .data_iq(dci_d), .valid_out(oq_v), .data_out(oq_d),
    .iqcor_enable(1'b1), .iqcor_coeff_1(q_c1), .iqcor_coeff_2(q_c2));

  // the dcfilter outputs are one stage ahead of the iqcor outputs: capture them in a FIFO
  logic [15:0] fifo_i[$], fifo_q[$];
  always @(posedge clk) if (dci_v === 1'b1) begin fifo_i.push_back(dci_d); fifo_q.push_back(dcq_d); end

  integer fin, fout, ratio, r, n_in, n_out;
  string  inname, outname;
  int dcenb_i, dccoeff_i, ic1, ic2, qc1, qc2;
  int vi, vq;
  logic [15:0] ci, cq;

  always @(posedge clk) if (oi_v === 1'b1) begin
    ci = fifo_i.pop_front(); cq = fifo_q.pop_front();
    $fwrite(fout, "%0d %0d %0d %0d\n", $signed(ci), $signed(cq), $signed(oi_d), $signed(oq_d));
    n_out++;
  end

  initial begin
    n_out = 0; n_in = 0;
    if (!$value$plusargs("in=%s", inname)) $fatal(1, "need +in=");
    if (!$value$plusargs("out=%s", outname)) $fatal(1, "need +out=");
    if (!$value$plusargs("dcenb=%d", dcenb_i)) dcenb_i = 0;
    if (!$value$plusargs("dccoeff=%d", dccoeff_i)) dccoeff_i = 0;
    if (!$value$plusargs("ic1=%d", ic1)) ic1 = 16384;
    if (!$value$plusargs("ic2=%d", ic2)) ic2 = 0;
    if (!$value$plusargs("qc1=%d", qc1)) qc1 = 0;
    if (!$value$plusargs("qc2=%d", qc2)) qc2 = 16384;
    if (!$value$plusargs("ratio=%d", ratio)) ratio = 1;
    dcfilt_enb = dcenb_i[0]; dcfilt_coeff = dccoeff_i[15:0];
    i_c1 = ic1[15:0]; i_c2 = ic2[15:0]; q_c1 = qc1[15:0]; q_c2 = qc2[15:0];
    fin = $fopen(inname, "r");
    fout = $fopen(outname, "w");
    repeat (8) @(negedge clk);
    while ($fscanf(fin, "%d %d", vi, vq) == 2) begin
      for (r = 0; r < ratio; r++) begin
        @(negedge clk);
        valid = (r == 0);
        d_i = vi[15:0]; d_q = vq[15:0];
      end
      n_in++;
    end
    @(negedge clk) valid = 0;
    repeat (40) @(negedge clk);
    $fclose(fout);
    $display("tb_adc_corr_vec: %0d in, %0d out", n_in, n_out);
    $finish;
  end
endmodule
