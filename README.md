# tryx-cli

Command-line and terminal-UI controller for TRYX cooler displays (Panorama SE,
Panorama, Turris 620) on Linux. No Qt, no daemon, one binary.

Status: pre-alpha. `tryx doctor` and `tryx devices` work; everything else is
still planned.

## Build

Enter the development shell (nix + direnv) and build:

```bash
direnv allow && cargo build
```

The shell provides `protoc`, `libusb`, and an `ffmpeg` with the `libx264`
encoder. Without nix, install those three yourself; `tryx doctor` reports
what is missing.

## Usage

```bash
tryx doctor            # ffmpeg/libx264, udev rules, group membership, connected displays
tryx devices [--json]  # connected TRYX displays and whether they are accessible
```

## Device access on Linux

Printer-class displays need the udev rules in `packaging/udev/` (copy them to
`/etc/udev/rules.d` and replug) or membership of the `lp` group. Displays still
on the legacy `cm01` firmware expose a serial port owned by `dialout` plus an
ADB interface; join `dialout` and install `adb`.

## Attribution

The protobuf schemas and udev rules are copied from
[DXVSI/Tryx-Linux-GUI](https://github.com/DXVSI/Tryx-Linux-GUI) (MIT), whose
protocol and media pipeline this project reproduces. See
[THIRD_PARTY.md](THIRD_PARTY.md).

## Licence

MIT, see [LICENSE](LICENSE).
