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
use `--offset-hz 0` for the capture center. The center is explicit rather than
inferred from the filename. Existing outputs are never overwritten.

`iq-fm` supports two-channel int16 IQ WAV above 192 kHz. It memory-maps the
input and processes the first 10 seconds by default, capped at 60 seconds.
It mixes the target to zero, resamples to 50 kHz, applies a 161-tap low-pass
FIR at 6250 Hz, takes the complex phase difference, removes mean DC, then
resamples to 48 kHz. `--bandwidth-hz` changes twice the low-pass cutoff.
The full channel plus a 2500 Hz transition guard must fit inside the capture.
Processing still needs memory proportional to the selected IQ duration.
Output gain uses a nominal 5 kHz FM deviation; filter startup and resampling
edges remain in the output.

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

- [ ] Get my ADALM-Pluto working.

Not started. Hardware setup and troubleshooting are for a later session.
