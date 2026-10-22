# earshot

This repository is public on GitHub (coryking/earshot). Everything committed here, including
commit messages, issues, PR descriptions and review comments, is published.

## Keep out of Git, issues and PRs

- People: no names of household members or anyone else. Say "a household member" or "the
  operator". Cory's own name and public GitHub handle are fine.
- The home network: no tailnet names (`*.ts.net` hosts), LAN addresses other than the Pluto's
  factory defaults, Wi-Fi details, or the house's location beyond "Seattle".
- Device identity: no hardware serial numbers, MAC addresses or keys. Write a placeholder such
  as `<your Pluto serial>`.
- Secrets: no passwords, tokens or `.env` contents. The Pluto's ADI factory defaults are public
  and fine.
- Off-air content: no recordings, IQ captures, decoded audio, or frame corpora cut from real
  transmissions, and no transcripts or summaries of what was said. They decode to other
  people's private traffic. Commit generated or synthetic test signals; keep real captures in
  the Git-ignored `recordings/` and `output/` directories, or Git-ignore them where they sit
  (see `engine/.gitignore`).
- Firmware: never commit the MD-380 image or RAM snapshot. `engine/scripts/fetch-md380.sh`
  downloads them into the Git-ignored `engine/.native-md380/`.

Before pushing, read the diff and the commit message as a stranger would. When something is
already published by mistake, tell Cory rather than rewriting public history.

Public data is fine: FCC ULS records, RadioReference-derived frequencies and labels, and
measurement results that contain no traffic content.

## Working here

`fpga/AGENTS.md` covers FPGA simulations, which need Cory's explicit approval.
