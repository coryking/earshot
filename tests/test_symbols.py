from __future__ import annotations

from contextlib import nullcontext
from pathlib import Path

import numpy as np
import pytest
import typer
from scipy.ndimage import gaussian_filter1d

from radio_tools import symbols
from radio_tools.symbols import (
    NXDN_FSW,
    SymbolResult,
    _save_output,
    analyze_iq,
    check_nxdn_frames,
    nxdn_crc7,
    recover_fsk,
)


def _bits(value: int, width: int) -> np.ndarray:
    return np.asarray([(value >> shift) & 1 for shift in range(width - 1, -1, -1)], dtype=np.uint8)


def _conv_encode(bits: np.ndarray) -> np.ndarray:
    state = 0
    encoded = []
    for bit in bits:
        register = (state << 1) | int(bit)
        encoded.extend(((register & 0x19).bit_count() % 2, (register & 0x17).bit_count() % 2))
        state = register & 0xF
    assert state == 0
    return np.asarray(encoded, dtype=np.uint8)


def make_nxdn_frame(*, bad_crc: bool = False, damage_fec: bool = False, lich: int = 0xC6):
    info = _bits(0x203FF2, 25)
    crc = nxdn_crc7(info)
    if bad_crc:
        crc ^= 1
    message = np.concatenate((info, _bits(crc, 7), np.zeros(4, dtype=np.uint8)))
    full_coded = _conv_encode(message)
    transmitted = full_coded[np.tile([1, 1, 1, 1, 1, 0], 12).astype(bool)]
    if damage_fec:
        transmitted[:24] ^= 1
    permutation = np.arange(60).reshape(5, 12).T.ravel()
    interleaved = transmitted[permutation]

    raw_bits = np.zeros(182 * 2, dtype=np.uint8)
    raw_bits[:16:2] = _bits(lich, 8)
    raw_bits[16:76] = interleaved
    payload_dibits = (raw_bits[0::2] << 1) | raw_bits[1::2]
    register = 0xE4
    scrambled = payload_dibits.copy()
    for i in range(len(scrambled)):
        bit = register & 1
        feedback = ((register >> 4) ^ register) & 1
        register = (register >> 1) | (feedback << 8)
        scrambled[i] ^= bit << 1

    sync_dibits = _bits(NXDN_FSW, 20).reshape(-1, 2)
    sync_dibits = (sync_dibits[:, 0] << 1) | sync_dibits[:, 1]
    return np.concatenate((sync_dibits, scrambled))


def test_known_scch_payload_passes_fsw_lich_crc_and_viterbi():
    dibits = make_nxdn_frame()

    hits, frames, total = check_nxdn_frames(dibits)

    assert total == 1
    assert hits[0].symbol_index == 0
    assert hits[0].word_hex == "0xcdf59"
    assert len(frames) == 1
    frame = frames[0]
    assert frame.lich_hex == "0xc6"
    assert frame.lich_parity_ok
    assert frame.lich_type == 0x63
    assert frame.type_d_eligible
    assert frame.scch_crc_ok is True
    assert frame.payload_hex == "101ff91c"
    assert frame.fec_hard_bit_cost == 0


def test_bad_scch_crc_and_corrupt_fec_are_reported_as_failed():
    _, bad_crc_frames, _ = check_nxdn_frames(make_nxdn_frame(bad_crc=True))
    _, damaged_frames, _ = check_nxdn_frames(make_nxdn_frame(damage_fec=True))

    assert bad_crc_frames[0].scch_crc_ok is False
    assert damaged_frames[0].fec_hard_bit_cost > 0
    assert damaged_frames[0].scch_crc_ok is False


def test_non_type_d_lich_is_not_labeled_with_type_d_scch_crc():
    _, frames, _ = check_nxdn_frames(make_nxdn_frame(lich=0x00))

    assert frames[0].lich_parity_ok
    assert frames[0].type_d_eligible is False
    assert frames[0].scch_crc_ok is None
    assert frames[0].payload_hex is None


def test_inverted_fsk_polarity_is_normalized_before_frame_check():
    inverted_dibits = np.asarray([2, 3, 0, 1], dtype=np.uint8)[make_nxdn_frame()]

    hits, frames, total = check_nxdn_frames(inverted_dibits)

    assert total == 1
    assert hits[0].polarity == "inverted"
    assert frames[0].polarity == "inverted"
    assert frames[0].scch_crc_ok is True


def test_four_level_fit_recovers_shaped_2400_baud_with_clock_offset():
    rng = np.random.default_rng(19)
    baud = 2400
    sample_rate = 48000
    sps = sample_rate // baud
    symbols = rng.choice(np.asarray([-3, -1, 1, 3]), size=900)
    shaped = np.repeat(symbols, sps).astype(np.float64) * 300
    shaped = gaussian_filter1d(shaped, sigma=2, mode="nearest")
    shaped = np.pad(shaped, (7, 0))
    shaped += rng.normal(0, 8, size=len(shaped))

    recovery = recover_fsk(shaped, sample_rate, baud)

    assert abs(recovery["estimated_baud"] - baud) <= baud * 0.02
    assert recovery["normalized_residual"] < 0.1
    assert recovery["level_spacing_hz"] > 2
    assert len(recovery["symbols"]) > 700
    assert set(np.unique(recovery["dibits"])) == {0, 1, 2, 3}


@pytest.mark.parametrize("baud", [299, 12001, float("nan")])
def test_recovery_rejects_baud_outside_supported_hypothesis_range(baud):
    with pytest.raises(ValueError, match="Baud"):
        recover_fsk(np.ones(4800), 48000, baud)


def test_absolute_target_requires_center_metadata_or_explicit_center(tmp_path, monkeypatch):
    class Recording:
        rate = 1_000_000
        samples = 500_000

        def read(self, start_s, seconds):
            return np.zeros((500_000, 2), dtype=np.int16)

        def center_at(self, start_s):
            return None

    monkeypatch.setattr(symbols, "open_iq", lambda _: nullcontext(Recording()))

    with pytest.raises(ValueError, match="needs SigMF center metadata or explicit --center-hz"):
        analyze_iq(
            tmp_path / "wide.wav",
            target_hz=454_337_000,
            offset_hz=None,
            center_hz=None,
            start_s=0,
            seconds=0.5,
            baud=2400,
            bandwidth_hz=6250,
        )


def test_optional_output_requires_confirmation_and_never_overwrites(tmp_path):
    result = SymbolResult(
        input="fixture.sigmf-meta",
        target_hz=454_337_000,
        center_hz=454_306_250,
        offset_hz=30_750,
        input_rate_hz=1_000_000,
        analyzed_s=0.5,
        requested_baud=2400,
        estimated_baud=2400,
        clock_phase_symbols=0.25,
        rrc_alpha=0.2,
        bandwidth_hz=6250,
        frequency_level_centers_hz=[-900, -300, 300, 900],
        level_spacing_hz=600,
        normalized_residual=0.01,
        symbols_count=10,
        nxdn_fsw_match_count=0,
        nxdn_fsw_hits=[],
        frame_checks=[],
        output_dir=None,
        notes=[],
    )
    arrays = {"symbols": np.arange(10, dtype=np.int8)}
    destination = tmp_path / "saved"

    with pytest.raises(typer.BadParameter, match="requires --yes"):
        _save_output(result, arrays, destination, yes=False)
    saved = _save_output(result, arrays, destination, yes=True)
    assert Path(saved.output_dir) == destination.resolve()
    assert (destination / "symbols.npz").is_file()
    assert (destination / "result.json").is_file()
    with pytest.raises(typer.BadParameter, match="already exists"):
        _save_output(result, arrays, destination, yes=True)
