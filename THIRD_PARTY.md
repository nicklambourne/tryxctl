# Third-party material

## DXVSI/Tryx-Linux-GUI (MIT)

<https://github.com/DXVSI/Tryx-Linux-GUI>, version 2.2.0.

Copied unchanged, with attribution headers:

- `crates/tryx-proto/proto/wire-v1/*.proto` — the project-owned clean-room
  protobuf schemas for the KANALI wire protocol.
- `packaging/udev/*.rules` — the two udev rules that grant seat access to the
  supported USB product IDs and stop CUPS from claiming them.

The Rust protocol, transport, and media code in this repository is written
from the behaviour documented and implemented there; the ffmpeg filter graphs
and encoder settings are reproduced deliberately so that prepared media is
interchangeable between the two projects.

## MrEssentials/tryx-linux-display-manager (GPL-3.0)

<https://github.com/MrEssentials/tryx-linux-display-manager>

Used only as a behavioural reference for the Turris 620 transfer path. No code
is copied from it.
