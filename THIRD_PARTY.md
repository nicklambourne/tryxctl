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

## AfroSamuraiX/panorama-manager (MIT)

<https://github.com/AfroSamuraiX/panorama-manager>

Its hardware notes on the legacy firmware informed several features: the fan
tachometer fields in the sysinfo reply, the `fanLCDSet` payload including the
vendor app's default smart-mode curve (copied as data into
`crates/tryx-legacy/src/commands.rs`), the `displayInSleep` and filter
semantics, the local-time offset on the sysinfo timestamp, and the
daemon-owns-the-port design.

## fadli0029/reed-tpse (MIT)

<https://github.com/fadli0029/reed-tpse>

The original reverse engineering of the legacy `cm01` serial protocol and its
keepalive, from which the DXVSI code we ported descends.
