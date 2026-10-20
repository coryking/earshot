import io
import json
import tarfile

import numpy as np
import pytest
from scipy.signal import welch
from typer.testing import CliRunner

from radio_tools.cli import app
from radio_tools.digital import iq_fm
from radio_tools.iq_recording import open_iq


def make_sigmf(tmp_path, *, gap=None, datatype="ci16_le", rate=100000, archive=True):
    t = np.arange(rate // 2) / rate
    z = 10000 * np.exp(1j * (2 * np.pi * 18000 * t + 2 * np.sin(2 * np.pi * 1200 * t)))
    data = np.column_stack([z.real, z.imag]).astype("<i2").tobytes()
    meta = {
        "global": {"core:sample_rate": rate, "core:datatype": datatype},
        "captures": [{"core:sample_start": 0, "core:frequency": 454300000}],
        "annotations": [],
    }
    if gap is not None:
        meta["annotations"].append({"core:label": "discontinuity", "core:sample_start": gap})
    if not archive:
        p = tmp_path / "capture.sigmf-meta"
        p.write_text(json.dumps(meta))
        p.with_suffix(".sigmf-data").write_bytes(data)
        return p
    p = tmp_path / "capture.sigmf.tar"
    with tarfile.open(p, "w") as tar:
        for name, payload in [
            ("capture.sigmf-meta", json.dumps(meta).encode()),
            ("capture.sigmf-data", data),
        ]:
            h = tarfile.TarInfo(name)
            h.size = len(payload)
            tar.addfile(h, io.BytesIO(payload))
    return p


@pytest.mark.parametrize("archive", [True, False])
def test_narrowband_sigmf_demodulates_with_metadata_center_and_time_range(tmp_path, archive):
    p = make_sigmf(tmp_path, archive=archive)
    with open_iq(p) as rec:
        assert rec.center_at(0.1) == 454300000
        assert rec.samples == 50000
    rate, duration, pcm = iq_fm(p, 0.2, 18000, start_s=0.1)
    assert rate == 100000
    assert duration == pytest.approx(0.2)
    f, power = welch(pcm[1000:].astype(float), 48000, nperseg=4096)
    assert f[power.argmax()] == pytest.approx(1200, abs=12)
    result = CliRunner().invoke(
        app,
        [
            "iq-fm",
            str(p),
            "--target-hz",
            "454318000",
            "--start-s",
            ".1",
            "--seconds",
            ".2",
            "--json",
            "--yes",
            "-o",
            str(tmp_path / "fm.wav"),
        ],
    )
    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout)["offset_hz"] == 18000


def test_sigmf_discontinuity_is_not_silently_joined(tmp_path):
    p = make_sigmf(tmp_path, gap=25000)
    with pytest.raises(ValueError, match="discontinuity"):
        iq_fm(p, 0.4, 18000)
    assert iq_fm(p, 0.1, 18000, start_s=0.3)[1] == pytest.approx(0.1)


@pytest.mark.parametrize("datatype", ["cf32_le", "cu8"])
def test_sigmf_format_must_not_be_guessed(tmp_path, datatype):
    with (
        pytest.raises(ValueError, match="ci16_le"),
        open_iq(make_sigmf(tmp_path, datatype=datatype)),
    ):
        pass


def test_sigmf_bounds_and_piped_archive(tmp_path):
    p = make_sigmf(tmp_path)
    with pytest.raises(ValueError, match="beyond"):
        iq_fm(p, 0.1, 0, start_s=1)
    with pytest.raises(ValueError, match="finite and nonnegative"):
        iq_fm(p, 0.1, 0, start_s=float("nan"))
    with pytest.raises(ValueError, match="Nyquist"):
        iq_fm(p, 0.1, 45000)
    # stdin is spooled under a WAV name: archive recognition must use content.
    copied = tmp_path / "stdin.wav"
    copied.write_bytes(p.read_bytes())
    assert iq_fm(copied, 0.1, 18000)[0] == 100000
