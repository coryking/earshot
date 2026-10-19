# Amateur radio

Amateur radio projects, experiments, and notes.

## Recording analysis

`radio` measures demodulated SDR audio, makes diagnostic plots, and optionally
probes POCSAG/FLEX with multimon-ng. It reports evidence; it does not identify a
protocol from its sound or a repeating waveform.

Run commands from this directory with [uv](https://docs.astral.sh/uv/):

```sh
uv sync
uv run radio --help
uv run radio recordings --limit 5 --json
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
Baseband/IQ is unsupported. SDR++ `baseband_` filenames and rates above 192 kHz
are rejected, but those checks cannot recognize every IQ recording. Do not pass
IQ data as audio by selecting one of its channels.

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
This tool does not decode DMR, P25, encrypted voice, or raw I/Q.

An FM-data capture generally needs an appropriate FM demodulator, enough
bandwidth, accurate tuning, and an unclipped audio level. Disable squelch,
deemphasis and audio noise reduction for diagnostic captures. `doctor` reads
SDR++'s saved `radio_config.json`; a saved AM selection does not prove that an
existing recording was made in AM.

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
selection, stdin, JSON output, errors, plot write guards, and decoder integration
using controlled subprocess fixtures. Live radio recordings and decoder binaries
stay out of Git.

## Hardware

- [ ] Get my ADALM-Pluto working.

Not started. Hardware setup and troubleshooting are for a later session.
