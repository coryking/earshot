# Amateur radio

Amateur radio projects, experiments, and notes.

## Recording analysis

`radio` measures demodulated SDR audio, channelizes baseband IQ, makes diagnostic
plots, and optionally decodes paging or DMR. It reports evidence; it does not identify a
protocol from its sound or a repeating waveform.

Run commands from this directory with [uv](https://docs.astral.sh/uv/):

```sh
uv sync
uv run radio --help
uv run radio recordings --limit 5 --json
uv run radio recordings --directory recordings --limit 5 --json
uv run radio doctor --json
```

The default SDR++ directory is `~/Library/Application Support/sdrpp` on macOS
and `$XDG_CONFIG_HOME/sdrpp` (or `~/.config/sdrpp`) elsewhere. `recordings` accepts
`--directory` to choose an audio directory; `doctor --directory` chooses the
configuration directory. Listing output is bounded by `--limit`, and `--fields
path,bytes` selects fields.

```sh
uv run radio inspect '/path/to/audio_454216000Hz_capture.wav' --seconds 10 --json
uv run radio plot '/path/to/audio.wav' --seconds 10 -o output/audio.png --yes
uv run radio paging '/path/to/audio.wav' --seconds 30 --json
cat '/path/to/audio.wav' | uv run radio inspect - --json
```

`inspect` reports the WAV sample rate, total and analyzed duration, RMS/peak
levels in dBFS, DC offset, near-full-scale samples, exact silence, spectral
peaks, and waveform autocorrelation peaks between 5 and 200 ms. Frequency in
`audio_<frequency>Hz...wav` is filename metadata, not a measured RF frequency.
Spectral levels are relative to the strongest Welch spectrum bin. Autocorrelation
uses the centered waveform and divides by zero-lag energy without correcting
for shrinking overlap. A 60 ms repeat is a timing observation, not proof of DMR.
Silence is reported as -300 dBFS to keep JSON finite.

Analysis defaults to the first 30 seconds, with `--seconds` capped at 300. The
WAV is loaded before truncation, so long files still need memory for the whole
recording. Mono integer PCM and float WAVs are supported. Multichannel audio
requires an explicit zero-based `--channel`; channels are never averaged.
The audio commands do not accept baseband/IQ. SDR++ `baseband_` filenames and rates above 192 kHz
are rejected, but those checks cannot recognize every IQ recording. Do not pass
IQ data as audio by selecting one of its channels. Use `iq-fm` first.

`plot` writes a waveform, spectrum and spectrogram PNG. It requires `--yes`,
never overwrites an existing file, and accepts `--dry-run` and `--json`.
Commands accept `-` for WAV stdin where a recording argument is expected.
Results go to stdout; errors and human-view inspection notes go to stderr.
Exit codes are 0 for success (including no decode), 1 for an operational error,
and 2 for bad command usage. There are no interactive prompts.

### Paging decoder

`paging` resamples the selected audio to 22,050 Hz signed 16-bit mono PCM and
runs POCSAG512, POCSAG1200, POCSAG2400, FLEX and FLEX_NEXT. It looks for an
executable in this order: `--decoder`, `RADIO_MULTIMON`, `PATH`, then this
checkout's `.tools/bin/multimon-ng`. An explicitly supplied missing executable
is an error. The optional binary is ignored by Git and is specific to the host.

`status: no_decode` means the decoder emitted no message; it does not rule out
paging or any other protocol. `decoder_output` means text was emitted, not that
its contents were independently verified. Output is limited to 16,000 characters.
The paging command does not decode DMR, P25, encrypted voice, or raw I/Q.

An FM-data capture generally needs an appropriate FM demodulator, enough
bandwidth, accurate tuning, and an unclipped audio level. Disable squelch,
deemphasis and audio noise reduction for diagnostic captures. `doctor` reads
SDR++'s saved `radio_config.json`; a saved AM selection does not prove that an
existing recording was made in AM.

### IQ to FM and DMR

Record SDR++ **Baseband**, container **WAV**, sample format **Int16**. The
capture contains I and Q as two channels. Keep the desired RF channels inside
the recorded span; audio recordings cannot reconstruct missing IQ information.

```sh
uv run radio iq-fm recordings/baseband_454217000Hz_14-39-04_19-10-2026.wav \
  --center-hz 454217000 --target-hz 454117000 --seconds 60 \
  -o output/fm-neighbor.wav --yes --json
uv run radio dmr output/fm-neighbor.wav --slot 2 --seconds 60 \
  -o output/dmr-slot2.wav --yes --json
```

Alternatively use `--offset-hz -100000`, where offset is target minus center;
use `--offset-hz 0` for the capture center. A WAV center must be explicit;
SigMF uses its capture metadata. Existing outputs are never overwritten.

`iq-fm` supports two-channel int16 IQ WAV above 192 kHz and `ci16_le` SigMF
metadata/data pairs or tar archives, including the app's 100 kHz slices. SigMF
provides the physical center for `--target-hz`; WAV requires `--center-hz` or
an explicit offset. Archive filenames are labels and may differ from the
physical center. Archives are read without extracting their contents.

It processes the first 10 seconds by default, capped at 60 seconds; `--start-s`
selects a later segment. WAV and standalone SigMF data are memory-mapped;
archive reads are capped at 256 MiB. A range crossing a SigMF capture boundary
or discontinuity is rejected. Select a shorter continuous segment instead.
It mixes the target to zero, resamples to 50 kHz, applies a 161-tap low-pass
FIR at 6250 Hz, takes the complex phase difference, removes mean DC, then
resamples to 48 kHz. `--bandwidth-hz` changes twice the low-pass cutoff.
The full channel plus a 2500 Hz transition guard must fit inside the capture.
Mixing and resampling use overlapped blocks of about one million input samples
instead of allocating the full IQ span. Rational resampling-grid alignment can
enlarge blocks for unusual sample rates. Only the smaller 50 kHz channel is
retained for the selected duration.
Output gain uses a nominal 5 kHz FM deviation; filter startup and resampling
edges remain in the output.

### Digital signalling and symbol recovery

Use `digital-probe` to inspect demodulated mono 48 kHz WAV with the managed
`dsd-fme` command. The search order is `--decoder`, `RADIO_DSD_FME`, then PATH.
The pinned installer lives in the dotfiles repository as
`run_onchange_install-dsd-fme.sh.tmpl`; it installs to `~/.local/bin/dsd-fme`
and statically links mbelib. It needs no project build directory or library-path
override. The Linux install and offline WAV input are verified; its Darwin
installation path has not yet been validated.

```sh
uv run radio iq-fm recordings/capture.sigmf.tar --target-hz 454337500 \
  --seconds 19 -o output/channel.wav --yes --json
uv run radio digital-probe output/channel.wav --protocol nxdn48 \
  --log-output output/channel.log --yes --json
uv run radio iq-symbols recordings/capture.sigmf.tar --target-hz 454337500 \
  --start-s 4.28 --seconds 0.44 --baud 2400 --json
```

`digital-probe` supports explicit `dmr`, `nxdn48`, `nxdn96`, `dpmr`, and
`p25p1` selections. Its default `auto` excludes NXDN and dPMR. Analysis is
capped at 120 seconds, with a 120-second decoder timeout. It uses a temporary
working directory and emits no playback audio. JSON distinguishes raw sync
reports, CRC failures, and selected CRC-gated NXDN message labels. Counts depend
on the pinned decoder's log format. `decoder_output` can include false syncs;
it is not proof of intelligible speech or an identified operator. Diagnostic
previews are bounded; `--log-output` saves up to 4 MiB to a new file.

`iq-symbols` tests a selected 4FSK baud hypothesis over 50 ms through 5 seconds,
fits four frequency levels, and checks full NXDN frame sync, LICH parity, and
eligible Type-D SCCH CRC7. It reports FEC correction cost and raw payloads.
Clock search spans ±2% of `--baud`; fit residual alone is not protocol evidence.
`--output-dir output/symbols --yes` saves a new directory containing JSON and
NPZ symbol arrays with stored-capture timestamps. IQ inputs match `iq-fm`, and
`-` accepts piped WAV or SigMF tar input.

See [verified capture findings and the non-audio UX handoff](docs/iq-analysis.md).

`dmr` requires 48 kHz demodulated audio and an optional DSDcc build with mbelib.
It sends signed 16-bit PCM to `dsdccx -i - -o - -fr -T <slot> -q` and wraps
emitted PCM in an 8 kHz mono WAV. Choose slot 1 or 2. `audio_emitted` means
the decoder produced samples; it does not verify speech intelligibility or
encryption status. `no_audio` is inconclusive. Synthesized audio can be shorter
than the input and is not aligned to the original timeline. Diagnostic text is
included in JSON, capped at 16,000 characters.

The decoder search order is `--decoder`, `RADIO_DSDCC`, `PATH`, then this
checkout's `.tools/bin/dsdccx`. The current Mac checkout has local copies of
DSDcc and mbelib, with shared-library references relative to the executable.
They do not depend on the temporary build directories. Provenance is in
`.tools/dsdcc/provenance.json`.

To rebuild locally with CMake and a C/C++ compiler already available:

```sh
mkdir -p .tools/src
git clone https://github.com/szechyjs/mbelib.git .tools/src/mbelib
git -C .tools/src/mbelib checkout 9a04ed5c78176a9965f3d43f7aa1b1f5330e771f
cmake -S .tools/src/mbelib -B .tools/src/mbelib/build \
  -DCMAKE_POLICY_VERSION_MINIMUM=3.5 -DCMAKE_INSTALL_PREFIX="$PWD/.tools/mbe"
cmake --build .tools/src/mbelib/build --parallel 4
cmake --install .tools/src/mbelib/build
git clone https://github.com/f4exb/dsdcc.git .tools/src/dsdcc
git -C .tools/src/dsdcc checkout f27b32d2df131ae3a376fe72d3fb880ae1f9ede1
cmake -S .tools/src/dsdcc -B .tools/src/dsdcc/build \
  -DCMAKE_POLICY_VERSION_MINIMUM=3.5 -DUSE_MBELIB=ON -DBUILD_TOOL=ON \
  -DLIBMBE_INCLUDE_DIR="$PWD/.tools/mbe/include" \
  -DLIBMBE_LIBRARY="$PWD/.tools/mbe/lib/libmbe.dylib"
cmake --build .tools/src/dsdcc/build --parallel 4
uv run radio dmr output/fm-neighbor.wav --slot 2 \
  --decoder "$PWD/.tools/src/dsdcc/build/dsdccx" -o output/rebuilt-slot2.wav --yes
```

The final library filename above is for macOS; Linux builds use `libmbe.so`.
The recorded revisions reproduce the tested source. Upstream documentation:
[DSDcc](https://github.com/f4exb/dsdcc), [mbelib](https://github.com/szechyjs/mbelib).

### Wideband IQ survey

Survey a recording before choosing channels to extract:

```sh
uv run radio iq-survey recordings/baseband_454217000Hz_capture.wav --json
uv run radio iq-survey recordings/baseband_454217000Hz_capture.wav \
  --seconds 60 --fft-size 32768 --windows 320 --limit 30 \
  -o output/survey.png --yes --json
```

`iq-survey` memory-maps a two-channel int16 IQ WAV and reads bounded FFT windows
spread evenly across the selected span. It defaults to 240 windows over the
first 30 seconds. The limits are 120 seconds, 512 windows, 65,536 FFT bins and
100 reported candidates. The default power matrix is about 16 MB; its maximum
is 128 MB, with additional working arrays. It does not load the entire capture
or create a derived spectrum dataset. Center frequency comes from `--center-hz`
or filename metadata; without either, results contain frequency offsets only.

The optional PNG shows mean sampled PSD, an estimated local floor and a sparse
waterfall on matching frequency axes. Density is dBFS/Hz relative to the signed
16-bit container: `I+jQ` divided by 32768, with unit complex magnitude at 0 dBFS.
The Hann PSD integrates to mean sampled signal power. These are not calibrated
RF dBm readings. Direct Pluto recordings can store raw 12-bit ADC values in the
16-bit container, while SDR++ can scale them differently; levels from those
paths are not directly comparable.

The local floor is a 100 kHz frequency median of each bin's temporal median
power divided by `ln(2)`, which estimates stationary white-noise power. Candidate
bins need mean power at least 8 dB above that floor and at least 5% occupancy
in the sampled windows. Adjacent detections separated by up to two FFT bins
are joined. Regions no wider than the larger of 1500 Hz or four FFT bins are
labelled `narrow_carrier`; wider ones are `occupied_region`. Separate broad-hump
candidates need 12 dB over a global floor and a contiguous width of at least
25 kHz or 4% of the captured span, whichever is larger. They may overlap smaller
candidates and may reflect receiver passband shape.

Each region's occupancy is the largest per-bin occupancy in that region, not
a measurement of total channel airtime. Narrow center/DC features and capture
edges receive artifact flags; wider regions crossing the capture center are
flagged separately. Candidate midpoints are extraction starting points rather
than exact channel assignments. The detector can split one transmission into
several regions, miss weak or brief bursts, or report receiver spurs and leakage.
Colored noise, dense occupancy and a nearly noiseless synthetic waveform can
distort its floor estimate. Every candidate remains unidentified until checked.

### Direct Pluto capture

`radio pluto capture` records receive-only IQ through libiio 0.x without the
SDR++ UI. Close SDR++ before capturing so only one process owns the receiver.
The default URI is `ip:pluto.local`; pass `--uri` for another libiio URI, including
the current USB URI. USB URIs can change after reconnection.

```sh
uv run radio pluto capture --center 454217000 --rate 5000000 --seconds 10 \
  --output recordings/baseband_454217000Hz_direct.wav --dry-run --json
uv run radio pluto capture --center 454217000 --rate 5000000 --seconds 10 \
  --output recordings/baseband_454217000Hz_direct.wav --yes --json
```

The preview checks bounded file size and free space without opening hardware.
Capture requires `--yes`, refuses existing WAV/sidecar paths, restores the
original receive rate, bandwidth, LO and gain mode on success or failure, and
writes metadata to `PATH.wav.json`. It preserves gain mode unless requested.
RX bandwidth defaults to the requested rate, capped at 56 MHz. No transmit
stream or transmit LO is opened.

On this Mac, the library defaults to SDR++'s bundled
`/Applications/SDR++.app/Contents/Frameworks/iio`; its bundled libusb is loaded
first. Default loading was verified without `DYLD_LIBRARY_PATH`. Other systems
can use an installed libiio 0.x library or `--library /path/to/library`.

Streaming is limited by the USB/backend throughput. The sidecar reports sample
duration, wall duration and throughput, and explicitly leaves continuity and
dropped-sample counts unknown. A single snapshot (`--single-buffer`) is capped
at 128 MiB and separates hardware sample duration from USB transfer time; for
example, `--rate 20000000 --seconds 0.25 --single-buffer` captures a 0.25-second
span, not a long-duration survey. These captures keep raw ADC values in a
signed 16-bit WAV container, normally with 12 valid bits.

### Building the paging decoder

The current Mac checkout has a local multimon-ng build. To reproduce it on a
host with Git, CMake and a C compiler already installed:

```sh
mkdir -p .tools/src .tools/bin
git clone https://github.com/EliasOenal/multimon-ng.git .tools/src/multimon-ng
git -C .tools/src/multimon-ng checkout b5f7d71c5e9b5a76093ba55c515968dc40f92804
cmake -S .tools/src/multimon-ng -B .tools/src/multimon-ng/build \
  -DX11_SUPPORT=OFF -DPULSE_AUDIO_SUPPORT=OFF -DSDL3_SCOPE=OFF -DBUILD_GEN_NG=OFF
cmake --build .tools/src/multimon-ng/build --target multimon-ng --parallel 4
cp .tools/src/multimon-ng/build/multimon-ng .tools/bin/multimon-ng
uv run radio doctor --json
```

The source revision records the build verified here; it is not a requirement
to stay on that revision. Keep the upstream GPL license and source available
with any redistributed binary. See [multimon-ng](https://github.com/EliasOenal/multimon-ng)
for supported decoders and build details.

### Development

```sh
uv sync --dev
uv run ruff check src tests
uv run ruff format --check src tests
uv run pytest -q
```

Tests cover numeric levels, known frequencies and repeated frames, channel
selection, stdin, JSON output, errors, plot write guards, synthetic offset FM
demodulation, IQ input bounds, and decoder integration
using controlled subprocess fixtures. Live radio recordings and decoder binaries
stay out of Git.

## Hardware

- [x] Receive IQ from ADALM-Pluto over USB with native libiio.

Continuous capture and receiver control readback were checked at 1.25, 2.5, 5 and
6 MS/s. The receive console restores the prior RX controls when it stops.

## Local radio monitor

The receive console keeps a catalog of detected RF channels and decoded DMR slots
and NXDN48 voice. **Monitor** pins one frequency and slot through silence; the
other channels continue decoding in parallel. Control/data carriers
do not count as voice. This version monitors fixed frequencies, not trunked
systems or talkgroups.

```sh
uv sync
npm ci --prefix web
npm run build --prefix web
# On this Mac, the installed SDR++ bundle supplies libiio and its dependencies:
DYLD_LIBRARY_PATH=/Applications/SDR++.app/Contents/Frameworks uv run radio-monitor
```

Open <http://127.0.0.1:8765>. Connect **Pluto live** using its libiio URI (the
current USB URI is editable in the console). **Receiver width** controls the actual
RX sample rate and RF bandwidth: 1.25, 2.5, 5 or 6 MHz. **Apply width** restarts RX
with readback and retains the catalog; channels outside the new span show out of
range. Click **Monitor** or **Listen** once to arm audio for new transmissions.
The console is live only. Historical files are available through the separate
analysis CLI, not through the console.

The source is receive-only and the server binds to loopback. No radio data is
uploaded. Live reception stays open continuously while analysis runs on a bounded
queue. The selected frequency and slot use a persistent channelizer and DSDcc
processes. Validated voice PCM streams directly over a local WebSocket to browser
WebAudio; selection and source changes flush queued sound. No historical audio
is replayed. Rolling protocol evidence gates the audio so false sync and control
frames do not unmute the speaker. The input-age display measures time since the
latest decoder input block, not an aligned end-to-end speech latency.

Background discovery independently surveys three-second IQ batches. Up to twelve
occupied narrow channels per batch are probed for DMR/NXDN48 in three workers.
New catalog voice requires protocol evidence and at least 0.4 seconds of emitted
PCM; known streams accept shorter validated endings. This background discovery
lag is displayed separately from the selected live stream. Emitted PCM is not
proof of intelligible or unencrypted speech.

The waterfall comes from actual IQ FFTs at up to eight rows per second, sent as
incremental rows to the browser on a 200 ms poll. It labels nominal frequency and
sample time. Power is relative to the int16 container, not calibrated RF dBm. USB
sample
continuity and dropped hardware buffers cannot be proven by this interface.
Decoder latency and skipped analysis batches appear below the waterfall. When
analysis cannot keep up, it drops the oldest queued batch instead of blocking
reception. Catalog rows remain when quiet; activity means recently detected RF or
recently validated decoded audio, rather than station identity.

**Stop** closes reception and restores the original sample rate, bandwidth, RX LO,
and gain mode, with readback checks. Receive/restoration failures appear in the
console. Stop SDR++ before connecting so it does not contend for the same RX
buffer. Closing the browser alone leaves the local service receiving; press Stop
or terminate the service to release the receiver. Temporary analysis IQ is
removed at shutdown; the console stores no voice playback history.

`RADIO_DATA_ROOT=/path/to/amateur-radio` points a worktree server at another local
checkout's `.tools/bin/dsdccx`.
`RADIO_DSDCC=/path/to/dsdccx` overrides the decoder. No decoder binary is bundled.
The backend API and its typed models are documented at `/docs`; frontend types
are generated from `/openapi.json` with `npm run types --prefix web` while the
server runs. Frontend development uses `npm run dev --prefix web` and proxies
`/api` to port 8765.

```sh
uv run pytest
npm test --prefix web
npm run build --prefix web
```

## Source synchronization

Source lives in the private [coryking/amateur-radio](https://github.com/coryking/amateur-radio)
repository. Both the Mac and Wigglebutt use `/Users/coryking/projects/amateur-radio`.
Push completed commits to GitHub, then update a clean Wigglebutt checkout with
`git pull --ff-only`. Keep unfinished work on its own branch/worktree.

Recordings, decoded audio, firmware backups, receiver captures, local decoder
builds, and generated output stay local. Git ignores `recordings/`, `output/`,
`.tools/`, native build directories, and capture/audio/firmware file types. Do
not force-add them. Use generated test signals for committed tests.
