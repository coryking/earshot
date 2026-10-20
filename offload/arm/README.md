# ARM C reference filter

What remains of the retired ARM producer: the C polyphase filter bank (`pfb.c`,
`prototype.c`) and the activity gate (`gate.c`). The FPGA model tests build them for
the host as a structural cross-check (`fpga/model/test_model.py`), and
`fpga/sw/pluto_fpga_source.c` uses the vendored jsmn header (MIT, from
https://github.com/zserge/jsmn; license kept in the header).

```sh
uv run pytest -q offload/arm/test_pfb.py
cc -O1 -g -fsanitize=address,undefined -Wall -Wextra -Werror \
  offload/arm/gate.c offload/arm/gate_test.c -lm -o /tmp/pluto-gate-test && /tmp/pluto-gate-test
```

The live receiver is the FPGA channelizer: `fpga/README.md` for the producer,
`supervisor/` for how it is kept running.
