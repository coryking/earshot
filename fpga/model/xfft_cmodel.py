"""ctypes binding to AMD's bit-accurate C model for the FFT LogiCORE (xfft v9.1, PG109).

The shared libraries ship with Vivado (not redistributed here). Set VIVADO_ROOT or
AMD_ROOT to the install root (the directory holding lnx64/tools); default ~/amd/2026.1.
"""

from __future__ import annotations

import ctypes
import os
from functools import lru_cache
from pathlib import Path

import numpy as np

ARCH = {"radix4_burst": 1, "radix2_burst": 2, "pipelined": 3, "radix2_lite": 4}


class _Generics(ctypes.Structure):
    _fields_ = [
        (name, ctypes.c_int)
        for name in (
            "C_NFFT_MAX", "C_ARCH", "C_HAS_NFFT", "C_USE_FLT_PT", "C_INPUT_WIDTH",
            "C_TWIDDLE_WIDTH", "C_HAS_SCALING", "C_HAS_BFP", "C_HAS_ROUNDING", "C_NSSR",
            "C_SYSTOLICFFT_INV",
        )
    ]


class _Inputs(ctypes.Structure):
    _fields_ = [
        ("nfft", ctypes.c_int),
        ("xn_re", ctypes.POINTER(ctypes.c_double)), ("xn_re_size", ctypes.c_int),
        ("xn_im", ctypes.POINTER(ctypes.c_double)), ("xn_im_size", ctypes.c_int),
        ("scaling_sch", ctypes.POINTER(ctypes.c_int)), ("scaling_sch_size", ctypes.c_int),
        ("direction", ctypes.c_int),
    ]


class _Outputs(ctypes.Structure):
    _fields_ = [
        ("xk_re", ctypes.POINTER(ctypes.c_double)), ("xk_re_size", ctypes.c_int),
        ("xk_im", ctypes.POINTER(ctypes.c_double)), ("xk_im_size", ctypes.c_int),
        ("blk_exp", ctypes.c_int), ("overflow", ctypes.c_int),
    ]


def _root() -> Path:
    for key in ("AMD_ROOT", "VIVADO_ROOT"):
        if os.environ.get(key):
            return Path(os.environ[key])
    return Path.home() / "amd" / "2026.1"


def library_path() -> str:
    """LD_LIBRARY_PATH entries the C model needs (its dependencies have no SONAME,
    so they cannot be satisfied by preloading; the dynamic loader must find them)."""
    tools = _root() / "lnx64" / "tools"
    return os.pathsep.join(str(tools / d) for d in ("fft_v9_1", "fpo_v7_1"))


def ensure_loader_path(argv: list[str]) -> None:
    """Re-exec the current interpreter once with LD_LIBRARY_PATH set for the C model."""
    need = library_path()
    if need in os.environ.get("LD_LIBRARY_PATH", ""):
        return
    env = dict(os.environ)
    env["LD_LIBRARY_PATH"] = need + os.pathsep + env.get("LD_LIBRARY_PATH", "")
    import sys

    os.execve(sys.executable, [sys.executable, *argv], env)


@lru_cache(maxsize=1)
def _lib() -> ctypes.CDLL:
    tools = _root() / "lnx64" / "tools"
    lib = ctypes.CDLL(str(tools / "fft_v9_1" / "libIp_xfft_v9_1_bitacc_cmodel.so"))
    lib.xilinx_ip_xfft_v9_1_create_state.restype = ctypes.c_void_p
    lib.xilinx_ip_xfft_v9_1_create_state.argtypes = [_Generics]
    lib.xilinx_ip_xfft_v9_1_destroy_state.argtypes = [ctypes.c_void_p]
    lib.xilinx_ip_xfft_v9_1_bitacc_simulate.restype = ctypes.c_int
    lib.xilinx_ip_xfft_v9_1_bitacc_simulate.argtypes = [
        ctypes.c_void_p, _Inputs, ctypes.POINTER(_Outputs)]
    return lib


def available() -> bool:
    try:
        _lib()
    except OSError:
        return False
    return True


class XFFT:
    """Fixed-point, unscaled, forward FFT matching an xfft v9.1 core configuration.

    Integer in, integer out: inputs are signed integers of input_width bits; outputs are
    signed integers of input_width + nfft + 1 bits (PG109 unscaled output width).
    """

    def __init__(self, nfft: int, input_width: int, twiddle_width: int,
                 arch: str = "pipelined", rounding: bool = False):
        g = _Generics(C_NFFT_MAX=nfft, C_ARCH=ARCH[arch], C_HAS_NFFT=0, C_USE_FLT_PT=0,
                      C_INPUT_WIDTH=input_width, C_TWIDDLE_WIDTH=twiddle_width,
                      C_HAS_SCALING=0, C_HAS_BFP=0, C_HAS_ROUNDING=int(rounding), C_NSSR=1,
                      C_SYSTOLICFFT_INV=0)
        self.n = 1 << nfft
        self.nfft = nfft
        self.input_width = input_width
        self.output_width = input_width + nfft + 1
        self._state = _lib().xilinx_ip_xfft_v9_1_create_state(g)
        if not self._state:
            raise RuntimeError("xfft C model rejected the configuration")

    def __del__(self):
        if getattr(self, "_state", None):
            _lib().xilinx_ip_xfft_v9_1_destroy_state(self._state)
            self._state = None

    def __call__(self, re: np.ndarray, im: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
        n = self.n
        scale = float(1 << (self.input_width - 1))
        lim = 1 << (self.input_width - 1)
        if re.shape != (n,) or im.shape != (n,):
            raise ValueError("one frame of n samples expected")
        if max(re.max(), im.max()) >= lim or min(re.min(), im.min()) < -lim:
            raise OverflowError("FFT input exceeds input_width")
        xr = np.ascontiguousarray(re, dtype=np.float64) / scale
        xi = np.ascontiguousarray(im, dtype=np.float64) / scale
        yr = np.zeros(n)
        yi = np.zeros(n)
        sch = (ctypes.c_int * 16)()
        stages = self.nfft  # unused when unscaled; size must still be plausible
        inp = _Inputs(self.nfft, xr.ctypes.data_as(ctypes.POINTER(ctypes.c_double)), n,
                      xi.ctypes.data_as(ctypes.POINTER(ctypes.c_double)), n, sch, stages, 1)
        out = _Outputs(yr.ctypes.data_as(ctypes.POINTER(ctypes.c_double)), n,
                       yi.ctypes.data_as(ctypes.POINTER(ctypes.c_double)), n, 0, 0)
        rc = _lib().xilinx_ip_xfft_v9_1_bitacc_simulate(self._state, inp, ctypes.byref(out))
        if rc:
            raise RuntimeError(f"xfft C model failed ({rc})")
        ir = yr * scale
        ii = yi * scale
        r = np.rint(ir).astype(np.int64)
        i = np.rint(ii).astype(np.int64)
        if np.any(r != ir) or np.any(i != ii):
            raise AssertionError("xfft C model output is not on the integer grid")
        return r, i
