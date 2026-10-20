# References

[P] means the primary document or an authoritative catalog record was checked.
[S] means only a secondary source was checked. "Unverified" marks details nobody
confirmed. The design note (`design.md`) cites these by key.

## Multirate DSP and channelizers

- **[harris04]** F. J. harris, *Multirate Signal Processing for Communication
  Systems*, Prentice Hall, 2004 (ISBN 978-0-13-146511-4) [P]. The 2nd edition is
  River Publishers, 2021. Ch. 6, Polyphase FIR filters: the polyphase partition of
  a prototype into M paths. Ch. 9, Polyphase channelizers. Rule-of-thumb length
  N ≈ (fs/Δf)·(A/22) [S: cited as 2nd ed. p. 59 by torchsig; page unverified].
- **[harris03]** F. J. harris, C. Dick, M. Rice, "Digital receivers and transmitters
  using polyphase filter banks for wireless communications," *IEEE Trans. MTT*
  51(4):1395–1412, 2003, doi:10.1109/TMTT.2003.809176 [P].
- **[harris10]** F. harris, R. McGwier, B. Egg, "A versatile multichannel filter
  bank with multiple channel bandwidths," CrownCom 2010,
  doi:10.4108/icst.crowncom2010.9238 [P citation]. Covers the M/2 (2x
  oversampled) polyphase channelizer.
- **[vallance17]** P. Vallance, "Channelization using RFNoC," GRCon 2017 [P]. M/2
  channelizer: a relaxed transition band, and a π phase step on odd bins per M/2
  shift, corrected by a circular shift or sign flip.
- **[crochiere83]** R. E. Crochiere, L. R. Rabiner, *Multirate Digital Signal
  Processing*, Prentice-Hall, 1983 (ISBN 0-13-605162-6) [P]. DFT filter banks; the
  chapter number is unverified.
- **[crochiere80]** R. E. Crochiere, "A weighted overlap-add method of short-time
  Fourier analysis/synthesis," *IEEE Trans. ASSP* 28(1):99–102, 1980 [P].
- **[kaiser74]** J. F. Kaiser, "Nonrecursive digital filter design using the
  I0-sinh window function," Proc. IEEE ISCAS 1974, pp. 20–23 [S].
  β = 0.1102(A − 8.7) for A > 50 dB; order n = (A − 7.95)/(2.285 Δω).
- **[pm72]** T. W. Parks, J. H. McClellan, "Chebyshev approximation for
  nonrecursive digital filters with linear phase," *IEEE Trans. Circuit Theory*
  19(2):189–194, 1972 [P].
- **[price16]** D. C. Price, "Spectrometers and polyphase filterbanks in radio
  astronomy," arXiv:1607.03579 [P].

## FFT

- **[wold84]** E. H. Wold, A. M. Despain, "Pipeline and parallel-pipeline FFT
  processors for VLSI implementations," *IEEE Trans. Computers* C-33(5):414–426,
  1984 [P]. R2SDF.
- **[he96]** S. He, M. Torkelson, "A new approach to pipeline FFT processor," IPPS
  1996, pp. 766–770, doi:10.1109/IPPS.1996.508145 [P]. R2²SDF.
- **[pg109]** AMD PG109, *Fast Fourier Transform LogiCORE IP Product Guide*, v9.1
  [P]:
  - Architectures: Pipelined Streaming, Radix-4/Radix-2 Burst, Radix-2 Lite.
  - Unscaled output width = input + log2 N + 1.
  - A bit-accurate C model is supplied.
  - Burst I/O with natural-order output runs load, process and unload serially.
  - Latency is reported in the IP GUI; there's no closed-form formula.

- **[xapp1161]** AMD/Xilinx XAPP1161, *Polyphase Filter Bank Channelizer*, Rev 1.0,
  2013 [P metadata; PDF not read]. A multi-channel FDM channelizer reference design.
  It was consulted only as to its existence and scope, so nothing here depends on
  it. **[xd100]** AMD Vitis Tutorial XD100, "Polyphase Channelizer" (AI Engine,
  Versal) [P]: a P/Q-oversampled channelizer after [harris03], M = 16, 8 taps per
  phase. It targets Versal AI Engines, so its architecture does not carry over.

## Open FPGA references

- **[casper]** A. Parsons et al., "PetaOp/second FPGA signal processing for SETI
  and radio astronomy," Asilomar 2006, doi:10.1109/ACSSC.2006.355123 [P]; Parsons et
  al., *PASP* 120:1207, 2008 [P]. The CASPER `pfb_fir_*` blocks hold coefficients in
  BRAM or distributed RAM [P].
- **[maia]** D. Estévez, Maia SDR (maia-sdr.org; github.com/maia-sdr/maia-sdr), FOSDEM
  2024 slides [P]. The FPGA (Amaranth) has an R2²SDF DIF FFT whose twiddle
  multiplier uses 3 DSPs. The 4096-point version is 2.2k LUT, 9.5 BRAM and 6 DSP,
  with a 3x DSP clock and a bit-exact Python model. It uses custom AXI3 DMA writers
  rather than axi_dmac, HP ports with manual cache invalidation, and a fixed
  62.5 MHz core clock. "Maia SDR DDC" (destevez.net, 2024-05-05) [P] describes a
  Parks–McClellan polyphase decimator designed on the ARM.
- **[adi]** Analog Devices hdl (hdl_2026_r1) [P]:
  - axi_dmac: AXI-Stream source (DMA_TYPE_SRC=1); FLAGS bits CYCLIC/TLAST/PARTIAL;
    partial-transfer reporting.
  - axi_ad9361: 16-bit sign-extended samples; `valid` at the per-channel rate.
  - util_cpack2; up_axi.
  - Common registers VERSION/ID/SCRATCH at DWORD 0x0/0x1/0x2.
  - Pluto project page.

## Spectral estimation and detection

- **[welch67]** P. D. Welch, "The use of fast Fourier transform for the estimation of
  power spectra…," *IEEE Trans. Audio Electroacoust.* 15(2):70–73, 1967 [P].
- **[harris78]** F. J. harris, "On the use of windows for harmonic analysis with the
  DFT," *Proc. IEEE* 66(1):51–83, 1978 [P]. ENBW is in Sec. II-A.
- **[percival93]** D. B. Percival, A. T. Walden, *Spectral Analysis for Physical
  Applications*, Cambridge, 1993. Equivalent degrees of freedom for overlapped
  Welch averages.
- **[urkowitz67]** H. Urkowitz, "Energy detection of unknown deterministic signals,"
  *Proc. IEEE* 55(4):523–531, 1967 [P citation]. Chi-square and noncentral
  chi-square statistics for the energy detector.
- **[kay98]** S. M. Kay, *Fundamentals of Statistical Signal Processing, Vol. II:
  Detection Theory*, Prentice Hall, 1998 [P].
- **[richards14]** M. A. Richards, *Fundamentals of Radar Signal Processing*, 2nd ed.,
  McGraw-Hill, 2014. CA-CFAR gives Pfa = (1 + α/N)^−N [S].
- **[rohling83]** H. Rohling, "Radar CFAR thresholding in clutter and multiple target
  situations," *IEEE Trans. AES* 19(4):608–621, 1983 [P]. OS-CFAR.
- **[martin01]** R. Martin, "Noise power spectral density estimation based on optimal
  smoothing and minimum statistics," *IEEE Trans. Speech Audio Process.*
  9(5):504–512, 2001. Minimum-statistics noise floor tracking. Citation from the
  author's knowledge; not checked by the research pass.

## Channel plan

- **[cfr90]** 47 CFR 90.209(b)(5), Table 1 [P]: 6.25 kHz channel spacing in
  406–512 MHz, and the 12.5 kHz narrowbanding deadline of 2013-01-01. 90.35(b)(3)
  [P]: 451–470 MHz I/B pool channels on a 6.25 kHz raster. 90.203(j) [P].
- **[dmr]** ETSI TS 102 361-1 V2.6.1 (2023-05) [P]:
  - 12.5 kHz carrier.
  - 4800 symbols/s 4FSK.
  - Outer-symbol deviation ±1.944 kHz.
  - RRC filtering.
  - Two TDMA slots.
- **[nxdn]** NXDN Forum technical specifications and white paper Rev. 04 [P]:
  6.25 kHz or 12.5 kHz FDMA 4FSK, 2400 symbols/s at 6.25 kHz.

## FPGA methodology (AMD docs, 2026.1 unless noted)

- **[ug949]** UltraFast Design Methodology Guide [P]:
  - Resets: don't code a reset by default; datapaths seldom need one; use
    synchronous resets; DSP48 and BRAM resets are synchronous-only.
  - CDC: use XPM_CDC / XPM_FIFO; `ASYNC_REG` on any custom synchronizer.
  - Covers report_methodology and check_timing.
- **[ug901]** Synthesis: DSP and BRAM inference coding styles, `USE_DSP`, `RAM_STYLE`.
- **[ug903]** Using Constraints [P]:
  - `set_max_delay -datapath_only` drops the hold check.
  - `set_clock_groups -asynchronous` only when every crossing is synchronized, and it
    overrides max-delay constraints.
  - False paths are one-directional and can hide real issues.
- **[ug906]** Design Analysis and Closure: report_cdc, methodology checks, clock
  interaction.
- **[ug953]** 7 Series / Zynq-7000 Libraries Guide: XPM_CDC, XPM_FIFO, XPM_MEMORY.
  UG974 covers UltraScale only.
- **[ug479]** 7 Series DSP48E1 (v1.10): 25×18 multiplier, 25-bit pre-adder,
  AREG/BREG/MREG/PREG.
- **[ug473]** 7 Series Memory Resources (v1.14): RAMB36 512×72 and RAMB18 512×36 in
  simple dual-port mode; READ_FIRST.
- **[pg101]** AXI Protocol Checker (AXI4/AXI3/AXI4-Lite). **[pg145]** AXI4-Stream
  Protocol Checker. **[pg267]** AXI Verification IP.
- **[ds190]** Zynq-7000 Overview: Z-7010 has 17,600 LUT, 35,200 FF, 60 RAMB36 and
  80 DSP48E1.
- **[ds187]** Zynq-7000 switching characteristics (-1):
  - DSP48E1 Fmax 464 MHz with all registers, 257 MHz without MREG.
  - BRAM 388 MHz.
- **[axi]** ARM IHI 0022H.c (AXI3/AXI4/AXI4-Lite; later issues are AXI5-only);
  ARM IHI 0051A/B (AXI4-Stream).
