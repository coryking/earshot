// Self-checking testbench for chan_top + xfft (the block-design datapath).
//
// Bit-accurate vectors come from fpga/model/golden.py; every history and selected-
// bin record is compared word-for-word against the model by frame number, so the
// checks still hold when backpressure drops whole frames. The AXI4-Lite bus and
// all four AXI4-Stream interfaces are watched by AMD protocol checker IP
// (PG101 / PG145); any assertion fails the test.
//
// Plusargs: VEC=<dir> plus the scenario values in <dir>/plusargs.txt.

`timescale 1ns/1ps

module tb_chan;

  // --------------------------------------------------------------------------
  // scenario
  string vec;
  int frames, samples, out_shift, ready_pct, adc_period_ps, adc_gap_pct;
  int expect_drops, expect_overflow, sat_samples, clipped, reset_at_frame;
  int on_q8, off_q8, hang, bias_q8, snapshot_at, auto_fwd, det_reset_at, adc_losses;
  int hist_first, hist_pairs, n_events;
  localparam int EVT_DEPTH = 16;   // small, so scenarios with many events overflow it
  bit h_lost = 0, s_lost = 0;    // a loss flag was seen: later records are not model-exact
  int errors = 0;

  logic [31:0] stim   [];
  logic [31:0] exp_bins [];
  logic [31:0] exp_level [0:511];
  logic [31:0] exp_active [0:15];
  logic [15:0] exp_psd [0:511];
  logic [31:0] mask_words [0:15];
  logic [41:0] exp_events [];
  bit          mask_bit [0:511];
  int          mask_count;

  // --------------------------------------------------------------------------
  // clocks and resets
  logic clk = 0, adc_clk = 0;
  logic aresetn = 0, adc_rst = 1;
  always #5 clk = ~clk;                          // 100 MHz
  initial begin
    wait (adc_period_ps > 0);
    forever #(adc_period_ps / 2000.0) adc_clk = ~adc_clk;
  end

  // --------------------------------------------------------------------------
  // DUT and xfft
  logic        awvalid = 0, wvalid = 0, bready = 0, arvalid = 0, rready = 0;
  logic [15:0] awaddr = 0, araddr = 0;
  logic [31:0] wdata = 0;
  wire         awready, wready, bvalid, arready, rvalid;
  wire  [1:0]  bresp, rresp;
  wire  [31:0] rdata;

  logic        adc_valid = 0;
  logic [15:0] adc_i = 0, adc_q = 0;

  wire [31:0] fft_in_data;   wire fft_in_valid, fft_in_ready, fft_in_last;
  wire [7:0]  cfg_data;      wire cfg_valid, cfg_ready;
  wire [63:0] fft_out_data;  wire [15:0] fft_out_user;
  wire        fft_out_valid, fft_out_ready, fft_out_last;
  wire        ev_unexp, ev_missing, ev_halt, fft_aresetn;
  wire [63:0] hist_data, sel_data;
  wire        hist_valid, sel_valid, hist_last, sel_last;
  logic       hist_ready = 0, sel_ready = 0;

  chan_top #(
    .ID(7), .COEF_FILE_PREFIX("pfb_coef_p"), .LOG_TABLE("chan_log2_table.mem"),
    .BLOCK_LOG2(2), .SUB_BLOCKS(2), .PSD_LOG2(3),  // = golden.SIM_DET
    .FIR2X(1), .EVT_DEPTH(EVT_DEPTH)
  ) dut (
    .s_axi_aclk (clk), .s_axi_aresetn (aresetn),
    .s_axi_awvalid (awvalid), .s_axi_awaddr (awaddr), .s_axi_awprot (3'd0),
    .s_axi_awready (awready), .s_axi_wvalid (wvalid), .s_axi_wdata (wdata),
    .s_axi_wstrb (4'hF), .s_axi_wready (wready), .s_axi_bvalid (bvalid),
    .s_axi_bresp (bresp), .s_axi_bready (bready), .s_axi_arvalid (arvalid),
    .s_axi_araddr (araddr), .s_axi_arprot (3'd0), .s_axi_arready (arready),
    .s_axi_rvalid (rvalid), .s_axi_rresp (rresp), .s_axi_rdata (rdata),
    .s_axi_rready (rready),
    .adc_clk (adc_clk), .adc_rst (adc_rst), .adc_valid (adc_valid),
    .adc_data_i (adc_i), .adc_data_q (adc_q),
    .m_axis_fft_tdata (fft_in_data), .m_axis_fft_tvalid (fft_in_valid),
    .m_axis_fft_tready (fft_in_ready), .m_axis_fft_tlast (fft_in_last),
    .m_axis_fftcfg_tdata (cfg_data), .m_axis_fftcfg_tvalid (cfg_valid),
    .m_axis_fftcfg_tready (cfg_ready),
    .s_axis_fftout_tdata (fft_out_data), .s_axis_fftout_tuser (fft_out_user),
    .s_axis_fftout_tvalid (fft_out_valid), .s_axis_fftout_tready (fft_out_ready),
    .s_axis_fftout_tlast (fft_out_last),
    .fft_event_tlast_unexpected (ev_unexp), .fft_event_tlast_missing (ev_missing),
    .fft_event_data_in_channel_halt (ev_halt), .fft_aresetn (fft_aresetn),
    .m_axis_hist_tdata (hist_data), .m_axis_hist_tvalid (hist_valid),
    .m_axis_hist_tready (hist_ready), .m_axis_hist_tlast (hist_last),
    .m_axis_sel_tdata (sel_data), .m_axis_sel_tvalid (sel_valid),
    .m_axis_sel_tready (sel_ready), .m_axis_sel_tlast (sel_last)
  );

  chan_xfft i_fft (
    .aclk (clk), .aresetn (fft_aresetn),
    .s_axis_config_tdata (cfg_data), .s_axis_config_tvalid (cfg_valid),
    .s_axis_config_tready (cfg_ready),
    .s_axis_data_tdata (fft_in_data), .s_axis_data_tvalid (fft_in_valid),
    .s_axis_data_tready (fft_in_ready), .s_axis_data_tlast (fft_in_last),
    .m_axis_data_tdata (fft_out_data), .m_axis_data_tuser (fft_out_user),
    .m_axis_data_tvalid (fft_out_valid), .m_axis_data_tready (fft_out_ready),
    .m_axis_data_tlast (fft_out_last),
    .event_frame_started (), .event_tlast_unexpected (ev_unexp),
    .event_tlast_missing (ev_missing), .event_status_channel_halt (),
    .event_data_in_channel_halt (ev_halt), .event_data_out_channel_halt ()
  );

  // --------------------------------------------------------------------------
  // protocol checkers (PG101, PG145)
  wire pc_lite, pc_in, pc_out, pc_hist, pc_sel;
  axi_pc_lite i_pc_lite (
    .aclk (clk), .aresetn (aresetn), .pc_status (), .pc_asserted (pc_lite),
    .pc_axi_awaddr (awaddr), .pc_axi_awprot (3'd0), .pc_axi_awvalid (awvalid),
    .pc_axi_awready (awready), .pc_axi_wdata (wdata), .pc_axi_wstrb (4'hF),
    .pc_axi_wvalid (wvalid), .pc_axi_wready (wready), .pc_axi_bresp (bresp),
    .pc_axi_bvalid (bvalid), .pc_axi_bready (bready), .pc_axi_araddr (araddr),
    .pc_axi_arprot (3'd0), .pc_axi_arvalid (arvalid), .pc_axi_arready (arready),
    .pc_axi_rdata (rdata), .pc_axi_rresp (rresp), .pc_axi_rvalid (rvalid),
    .pc_axi_rready (rready));
  axis_pc_fftin i_pc_in (
    .aclk (clk), .aresetn (fft_aresetn), .pc_axis_tvalid (fft_in_valid),
    .pc_axis_tready (fft_in_ready), .pc_axis_tdata (fft_in_data),
    .pc_axis_tlast (fft_in_last), .pc_asserted (pc_in), .pc_status ());
  axis_pc_fftout i_pc_out (
    .aclk (clk), .aresetn (fft_aresetn), .pc_axis_tvalid (fft_out_valid),
    .pc_axis_tready (fft_out_ready), .pc_axis_tdata (fft_out_data),
    .pc_axis_tlast (fft_out_last), .pc_axis_tuser (fft_out_user),
    .pc_asserted (pc_out), .pc_status ());
  axis_pc_hist i_pc_hist (
    .aclk (clk), .aresetn (aresetn), .pc_axis_tvalid (hist_valid),
    .pc_axis_tready (hist_ready), .pc_axis_tdata (hist_data),
    .pc_axis_tlast (hist_last), .pc_asserted (pc_hist), .pc_status ());
  axis_pc_sel i_pc_sel (
    .aclk (clk), .aresetn (aresetn), .pc_axis_tvalid (sel_valid),
    .pc_axis_tready (sel_ready), .pc_axis_tdata (sel_data),
    .pc_axis_tlast (sel_last), .pc_asserted (pc_sel), .pc_status ());

  always @(posedge clk) begin
    if (pc_lite | pc_in | pc_out | pc_hist | pc_sel) begin
      $error("protocol checker asserted: lite=%0d fftin=%0d fftout=%0d hist=%0d sel=%0d",
             pc_lite, pc_in, pc_out, pc_hist, pc_sel);
      errors++;
    end
  end

  // --------------------------------------------------------------------------
  // AXI4-Lite master
  task automatic axi_write(input [15:0] addr, input [31:0] data);
    @(posedge clk);
    awaddr <= addr; wdata <= data; awvalid <= 1; wvalid <= 1;
    fork
      begin do @(posedge clk); while (!awready); awvalid <= 0; end
      begin do @(posedge clk); while (!wready);  wvalid <= 0; end
    join
    bready <= 1;
    do @(posedge clk); while (!bvalid);
    if (bresp != 2'b00) begin $error("BRESP %0d at %h", bresp, addr); errors++; end
    bready <= 0;
  endtask

  task automatic axi_read(input [15:0] addr, output [31:0] data);
    @(posedge clk);
    araddr <= addr; arvalid <= 1;
    do @(posedge clk); while (!arready);
    arvalid <= 0;
    rready <= 1;
    do @(posedge clk); while (!rvalid);
    data = rdata;
    if (rresp != 2'b00) begin $error("RRESP %0d at %h", rresp, addr); errors++; end
    rready <= 0;
  endtask

  function automatic [15:0] reg_addr(input int dword);
    return dword[13:0] << 2;
  endfunction

  // --------------------------------------------------------------------------
  // stream sinks with random backpressure
  always @(posedge clk) begin
    hist_ready <= ($urandom_range(99) < ready_pct);
    sel_ready  <= ($urandom_range(99) < ready_pct);
  end

  // History records: header + hist_pairs words of the window that starts at bin
  // hist_first (wrapping), checked against exp_bins[] by frame number.
  function automatic int wbin(input int i);
    return (hist_first + i) % 512;
  endfunction
  int     h_word = -1;          // -1: expecting header
  longint h_frame, h_last = -1;
  int     h_records = 0, h_disc_flags = 0, h_ovf_flags = 0, h_sat_flags = 0, h_gaps = 0;
  always @(posedge clk) if (hist_valid & hist_ready) begin
    if (h_word < 0) begin
      if (hist_data[63:56] != 8'hA1) begin
        $error("hist: expected header, got %h", hist_data); errors++;
      end else begin
        h_frame = hist_data[47:0];
        if (hist_data[48]) h_disc_flags++;
        if (hist_data[49]) begin h_ovf_flags++; h_lost = 1; end
        if (hist_data[50]) h_sat_flags++;
        if (h_frame != h_last + 1 && h_frame != 0) begin
          h_gaps++;
          if (!hist_data[48]) begin
            $error("hist: frame %0d follows %0d without the discontinuity flag", h_frame, h_last);
            errors++;
          end
        end
        h_last = h_frame;
        h_word = 0;
      end
    end else begin
      if (!h_lost && h_frame < frames &&
          hist_data !== {exp_bins[h_frame*512 + wbin(2*h_word + 1)], exp_bins[h_frame*512 + wbin(2*h_word)]}) begin
        if (errors < 20) $error("hist f%0d p%0d: got %h expected %h", h_frame, h_word, hist_data,
               {exp_bins[h_frame*512 + wbin(2*h_word + 1)], exp_bins[h_frame*512 + wbin(2*h_word)]});
        errors++;
      end
      h_word++;
      if (h_word == hist_pairs) begin h_word = -1; h_records++; end
    end
  end

  // Selected-bin records: header, then exactly the masked bins in walk order (from
  // hist_first, wrapping).
  longint s_frame = -1, s_last = -1;
  int     s_prev_bin, s_count, s_records = 0, s_gaps = 0;
  task automatic sel_close();
    if (s_frame >= 0 && (auto_fwd ? s_count < mask_count : s_count != mask_count)) begin
      $error("sel f%0d: %0d samples, expected %0d", s_frame, s_count, mask_count); errors++;
    end
  endtask
  always @(posedge clk) if (sel_valid & sel_ready) begin
    if (sel_data[63:56] == 8'hA2) begin
      sel_close();
      s_frame = sel_data[47:0];
      if (s_frame != s_last + 1 && s_frame != 0) begin
        s_gaps++;
        if (!sel_data[48]) begin $error("sel: gap without flag at %0d", s_frame); errors++; end
      end
      if (sel_data[49]) s_lost = 1;
      s_last = s_frame; s_prev_bin = -1; s_count = 0; s_records++;
    end else if (sel_data[63:60] == 4'h5) begin
      automatic int bn = sel_data[56:48];
      automatic int pos = (bn - hist_first + 512) % 512;
      if (s_frame < 0 || pos <= s_prev_bin || (!mask_bit[bn] && !auto_fwd) ||
          sel_data[47:32] != s_frame[15:0]) begin
        $error("sel: bad sample word %h in frame %0d", sel_data, s_frame); errors++;
      end else if (!s_lost && s_frame < frames && sel_data[31:0] !== exp_bins[s_frame*512 + bn]) begin
        if (errors < 20) $error("sel f%0d bin %0d: got %h expected %h", s_frame, bn,
                                sel_data[31:0], exp_bins[s_frame*512 + bn]);
        errors++;
      end
      s_prev_bin = pos; s_count++;
    end else begin
      $error("sel: unknown word %h", sel_data); errors++;
    end
  end

  // --------------------------------------------------------------------------
  // ADC feeder
  bit feed_go = 0, feed_done = 0;
  initial begin
    wait (feed_go);
    forever begin
      for (int n = 0; n < samples; ) begin
        @(posedge adc_clk);
        if ($urandom_range(99) < adc_gap_pct) begin
          adc_valid <= 0;
        end else begin
          adc_valid <= 1;
          adc_i <= stim[n][15:0];
          adc_q <= stim[n][31:16];
          n++;
        end
      end
      @(posedge adc_clk) adc_valid <= 0;
      feed_done = 1;
      wait (!feed_done);
    end
  end

  // --------------------------------------------------------------------------
  task automatic configure();
    axi_write(reg_addr(6), out_shift);
    axi_write(reg_addr(8), on_q8);
    axi_write(reg_addr(9), off_q8);
    axi_write(reg_addr(10), bias_q8);
    axi_write(reg_addr(11), hang);
    for (int w = 0; w < 16; w++) axi_write(reg_addr('h40 + w), mask_words[w]);
    axi_write(reg_addr('h28), (hist_pairs << 16) | hist_first);
  endtask

  task automatic wait_locked();
    logic [31:0] v;
    int n = 0;
    do begin
      repeat (100) @(posedge clk);
      axi_read(reg_addr(5), v);
      n++;
    end while (!v[4] && n < 1000);
    if (!v[4]) begin $error("FIR clock never locked"); errors++; end
  endtask

  // Activity events. Exact against the model where the detector saw an unbroken
  // run (no resets, no lost samples); otherwise only well-formedness. The FIFO keeps
  // the oldest EVT_DEPTH events and counts the rest as lost.
  task automatic check_events();
    logic [31:0] st, head, fr;
    int kept, exact, bad = 0;
    longint last_fr = -1;
    exact = (reset_at_frame < 0 && det_reset_at < 0 && !expect_overflow && snapshot_at < 0);
    axi_read(reg_addr('h24), st);
    kept = st[15:0];
    $display("  events: %0d queued, %0d lost, %0d expected", kept, st[31:16], n_events);
    if (exact) begin
      if (kept != (n_events < EVT_DEPTH ? n_events : EVT_DEPTH) ||
          st[31:16] != (n_events > EVT_DEPTH ? n_events - EVT_DEPTH : 0)) begin
        $error("event count/lost mismatch"); errors++;
      end
    end
    for (int i = 0; i < kept; i++) begin
      axi_read(reg_addr('h25), head);
      axi_read(reg_addr('h26), fr);
      if (!head[31]) begin $error("event %0d: head not valid", i); bad++; end
      if (exact && {head[30], head[8:0], fr} !== exp_events[i]) begin
        if (bad < 5) $error("event %0d: got open=%0d bin=%0d frame=%0d, expected %h", i,
                            head[30], head[8:0], fr, exp_events[i]);
        bad++;
      end
      if (fr < last_fr) begin $error("event %0d: frame %0d before %0d", i, fr, last_fr); bad++; end
      last_fr = fr;
      axi_write(reg_addr('h27), 1);
    end
    check_reg('h25, "evt_head@drained", 0);
    axi_read(reg_addr('h24), st);
    if (st[15:0] != 0) begin $error("events left after draining: %0d", st[15:0]); bad++; end
    errors += bad;
  endtask

  task automatic wait_frames(input int target);
    logic [31:0] v, prev;
    int still = 0;
    prev = 0;
    forever begin
      repeat (2000) @(posedge clk);
      axi_read(reg_addr('h12), v);
      if (v >= target) break;
      if (v == prev) still++; else still = 0;
      if (still > 20) begin
        if (!expect_overflow) begin
          $error("stalled at %0d of %0d frames", v, target); errors++;
        end
        break;
      end
      prev = v;
    end
    // let the packetizer finish and the output FIFOs drain to the (slow) sinks
    for (int i = 0; i < 2000; i++) begin
      repeat (1000) @(posedge clk);
      axi_read(reg_addr(5), v);
      if (v[2:1] == 2'b11 && !v[3]) break;
    end
    repeat (200) @(posedge clk);
  endtask

  function automatic [15:0] clip_sext(input [15:0] v);
    if ($signed(v) > 2047) return 16'd2047;
    if ($signed(v) < -2048) return 16'hF800;
    return v;
  endfunction

  // Live self-test path: freeze, read back the frame's exact input window and FFT
  // output, compare both with the model, then resume.
  task automatic snapshot_check();
    logic [31:0] v, sf, expected;
    int bad = 0;
    wait_frames(snapshot_at);
    axi_write(reg_addr('h22), 1);
    do begin
      repeat (500) @(posedge clk);
      axi_read(reg_addr('h22), v);
    end while (v[2:1] != 2'b11);
    axi_read(reg_addr('h23), sf);
    for (int i = 0; i < 4096; i++) begin
      axi_read(reg_addr('h1000 + i), v);
      expected = {clip_sext(stim[256*sf + i][31:16]), clip_sext(stim[256*sf + i][15:0])};
      if (v !== expected) begin
        if (bad < 5) $error("snapshot input %0d: %h expected %h", i, v, expected);
        bad++;
      end
    end
    for (int k = 0; k < 512; k++) begin
      axi_read(reg_addr('h600 + k), v);
      if (v !== exp_bins[sf*512 + k]) begin
        if (bad < 10) $error("snapshot bin %0d: %h expected %h", k, v, exp_bins[sf*512 + k]);
        bad++;
      end
    end
    $display("  snapshot of frame %0d: %0d mismatches (4096 inputs, 512 bins)", sf, bad);
    errors += bad;
    axi_write(reg_addr('h22), 0);
  endtask

  task automatic check_reg(input int dword, input string name, input logic [31:0] expected);
    logic [31:0] v;
    axi_read(reg_addr(dword), v);
    if (v !== expected) begin
      $error("%s = %0d (0x%h), expected %0d", name, v, v, expected); errors++;
    end else $display("  %-16s = %0d", name, v);
  endtask

  initial begin
    logic [31:0] v;
    if (!$value$plusargs("VEC=%s", vec)) $fatal(1, "missing +VEC");
    void'($value$plusargs("FRAMES=%d", frames));
    void'($value$plusargs("SAMPLES=%d", samples));
    void'($value$plusargs("OUT_SHIFT=%d", out_shift));
    void'($value$plusargs("READY_PCT=%d", ready_pct));
    void'($value$plusargs("ADC_PERIOD_PS=%d", adc_period_ps));
    void'($value$plusargs("ADC_GAP_PCT=%d", adc_gap_pct));
    void'($value$plusargs("EXPECT_DROPS=%d", expect_drops));
    void'($value$plusargs("EXPECT_OVERFLOW=%d", expect_overflow));
    void'($value$plusargs("SAT_SAMPLES=%d", sat_samples));
    void'($value$plusargs("CLIPPED=%d", clipped));
    void'($value$plusargs("RESET_AT_FRAME=%d", reset_at_frame));
    void'($value$plusargs("ON_Q8=%d", on_q8));
    void'($value$plusargs("OFF_Q8=%d", off_q8));
    void'($value$plusargs("HANG=%d", hang));
    void'($value$plusargs("BIAS_Q8=%d", bias_q8));
    snapshot_at = -1; auto_fwd = 0; det_reset_at = -1; adc_losses = 0;
    void'($value$plusargs("AUTO_FWD=%d", auto_fwd));
    void'($value$plusargs("DET_RESET_AT_FRAME=%d", det_reset_at));
    void'($value$plusargs("ADC_LOSSES=%d", adc_losses));
    void'($value$plusargs("SNAPSHOT_AT_FRAME=%d", snapshot_at));
    hist_first = 0; hist_pairs = 256; n_events = 0;
    void'($value$plusargs("HIST_FIRST=%d", hist_first));
    void'($value$plusargs("HIST_PAIRS=%d", hist_pairs));
    void'($value$plusargs("EVENTS=%d", n_events));
    exp_events = new[n_events > 0 ? n_events : 1];
    $readmemh({vec, "/events.hex"}, exp_events);
    stim = new[samples];
    exp_bins = new[frames * 512];
    $readmemh({vec, "/stim.hex"}, stim);
    $readmemh({vec, "/bins.hex"}, exp_bins);
    $readmemh({vec, "/level.hex"}, exp_level);
    $readmemh({vec, "/active.hex"}, exp_active);
    $readmemh({vec, "/psd.hex"}, exp_psd);
    $readmemh({vec, "/mask.hex"}, mask_words);
    mask_count = 0;
    for (int k = 0; k < 512; k++) begin
      mask_bit[k] = mask_words[k / 32][k % 32];
      mask_count += mask_bit[k];
    end
    $display("tb_chan: %s frames=%0d samples=%0d ready=%0d%% adc=%0dps", vec, frames,
             samples, ready_pct, adc_period_ps);

    repeat (20) @(posedge clk);
    aresetn <= 1;
    repeat (5) @(posedge adc_clk);
    adc_rst <= 0;
    repeat (50) @(posedge clk);
    wait_locked();

    axi_read(reg_addr(0), v);  $display("  VERSION 0x%h", v);
    axi_read(reg_addr(3), v);
    if (v != 32'h4348414E) begin $error("MAGIC 0x%h", v); errors++; end
    axi_write(reg_addr(2), 32'h5A5A1234);
    axi_read(reg_addr(2), v);
    if (v != 32'h5A5A1234) begin $error("SCRATCH 0x%h", v); errors++; end

    // reset defaults, then configuration readback
    check_reg(4, "control@reset", 0);
    check_reg(6, "out_shift@reset", 7);
    check_reg(8, "det_on@reset", 221);
    check_reg(9, "det_off@reset", 133);
    check_reg(11, "det_hang@reset", 39);
    check_reg(1, "id", 7);
    check_reg(7, "config", {8'd2, 8'd8, 16'd512});
    check_reg('hC, "det_config", {8'd3, 8'd4, 16'd2});
    check_reg('h28, "hist_window@reset", {7'd0, 9'd256, 7'd0, 9'd0});
    check_reg('h24, "evt_status@reset", 0);
    check_reg('h25, "evt_head@reset", 0);
    configure();
    check_reg(6, "out_shift", out_shift);
    check_reg(8, "det_on", on_q8);
    check_reg(9, "det_off", off_q8);
    check_reg(10, "det_bias", bias_q8);
    check_reg(11, "det_hang", hang);
    for (int w = 0; w < 16; w++) check_reg('h40 + w, "mask", mask_words[w]);
    axi_write(reg_addr(4), auto_fwd ? 32'hF : 32'h7);  // run, hist_en, sel_en[, auto]
    repeat (100) @(posedge clk);
    feed_go = 1;

    if (det_reset_at > 0) begin
      wait_frames(det_reset_at);
      axi_write(reg_addr(4), 32'h1F);              // DET_RESET pulse while running
      check_reg(4, "control", 32'hF);
    end

    if (reset_at_frame > 0) begin
      // (1) soft stop while the packetizer is mid-record: the record must complete
      wait_frames(reset_at_frame);
      wait (dut.i_pack.busy && dut.i_pack.hist_go && dut.i_pack.b > 100);
      axi_write(reg_addr(4), 32'h6);
      repeat (3000) @(posedge clk);
      axi_read(reg_addr(5), v);
      if (v[0] || v[3]) begin $error("datapath still running/packing after stop: %h", v); errors++; end
      // (2) system and ADC-core reset while samples are still streaming in
      @(posedge clk) aresetn <= 0;
      @(posedge adc_clk) adc_rst <= 1;
      repeat (16) @(posedge clk);
      aresetn <= 1;
      @(posedge adc_clk) adc_rst <= 0;
      repeat (50) @(posedge clk);
      check_reg(4, "control@sysreset", 0);
      check_reg('h28, "hist_window@sysreset", {7'd0, 9'd256, 7'd0, 9'd0});
      check_reg('h24, "evt_status@sysreset", 0);
      wait_locked();
      check_reg(6, "out_shift@sysreset", 7);
      check_reg('h12, "frames@sysreset", 0);
      if (h_word != -1) begin $error("hist record truncated by the stop (word %0d)", h_word); errors++; end
      wait (feed_done);
      h_last = -1; s_last = -1;
      configure();
      axi_write(reg_addr(4), 32'h7);
      feed_done = 0;
    end

    wait (feed_done);
    wait_frames(frames);

    $display("results:");
    axi_read(reg_addr('h12), v);  $display("  frames_out       = %0d", v);
    axi_read(reg_addr('h10), v);  $display("  samples          = %0d", v);
    if (!expect_overflow) begin
      check_reg('h12, "frames_out", frames);
      check_reg('h10, "samples", samples);
      check_reg('h15, "clips", clipped);
      check_reg('h16, "saturations", sat_samples);
      check_reg('h14, "adc_losses", adc_losses);
    end else begin
      axi_read(reg_addr('h14), v);
      $display("  adc_overflows    = %0d", v);
      if (v == 0) begin $error("expected ADC overflows"); errors++; end
      if (h_ovf_flags == 0) begin $error("no record carried the overflow flag"); errors++; end
    end
    check_reg('h20, "fft_events", 0);
    check_reg('h1B, "pack_overruns", 0);
    axi_read(reg_addr('h18), v);  $display("  hist_drops       = %0d", v);
    if (!expect_drops && v != 0) begin $error("unexpected hist drops"); errors++; end
    if (expect_drops && v == 0 && !expect_overflow) begin $error("expected hist drops"); errors++; end
    axi_read(reg_addr('h1A), v);  $display("  sel_drops        = %0d", v);
    if (!expect_drops && v != 0) begin $error("unexpected sel drops"); errors++; end
    sel_close();
    if (h_word != -1) begin $error("partial history record at end (word %0d)", h_word); errors++; end
    $display("  hist records %0d (gaps %0d), sel records %0d (gaps %0d), sat flags %0d",
             h_records, h_gaps, s_records, s_gaps, h_sat_flags);
    if (!expect_drops && !expect_overflow && reset_at_frame < 0 && snapshot_at < 0) begin
      if (h_records != frames || s_records != frames) begin
        $error("records hist %0d sel %0d, expected %0d each", h_records, s_records, frames); errors++;
      end
    end else if (reset_at_frame < 0 && snapshot_at < 0) begin
      axi_read(reg_addr('h17), v);
      if (h_records != v) begin $error("hist records %0d != HIST_FRAMES %0d", h_records, v); errors++; end
      axi_read(reg_addr('h19), v);
      if (s_records != v) begin $error("sel records %0d != SEL_FRAMES %0d", s_records, v); errors++; end
    end
    if (h_records == 0 || s_records == 0) begin $error("no records"); errors++; end
    check_reg('h2C, "hist_window", {7'd0, hist_pairs[8:0], 7'd0, hist_first[8:0]});
    if (reset_at_frame < 0) begin
      // ring position registers: everything admitted since arming has drained
      check_reg('h29, "hist_records", h_records);
      check_reg('h2A, "hist_last_frame", h_last[31:0]);
      check_reg('h2B, "hist_done", h_records);
    end
    check_events();
    if (sat_samples > 0 && h_sat_flags == 0) begin $error("no saturation flag"); errors++; end

    if (!expect_overflow && det_reset_at < 0) begin
      int lv_err = 0;
      for (int k = 0; k < 512; k++) begin
        axi_read(reg_addr('h200 + k), v);
        if (v !== exp_level[k]) begin
          if (lv_err < 5) $error("LEVEL[%0d] = %h expected %h", k, v, exp_level[k]);
          lv_err++;
        end
        axi_read(reg_addr('h400 + k), v);
        if (v[15:0] !== exp_psd[k]) begin
          if (lv_err < 5) $error("PSD[%0d] = %h expected %h", k, v[15:0], exp_psd[k]);
          lv_err++;
        end
      end
      for (int w = 0; w < 16; w++) begin
        axi_read(reg_addr('h50 + w), v);
        if (v !== exp_active[w]) begin
          $error("ACTIVE[%0d] = %h expected %h", w, v, exp_active[w]); lv_err++;
        end
      end
      $display("  level/psd/active mismatches: %0d", lv_err);
      errors += lv_err;
    end

    if (errors == 0) $display("TB_CHAN PASS");
    else $display("TB_CHAN FAIL errors=%0d", errors);
    $finish;
  end

  initial begin
    #200ms;
    $display("TB_CHAN FAIL timeout");
    $finish;
  end

endmodule
