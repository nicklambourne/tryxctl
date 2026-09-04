# tryx-cli

Command-line and terminal-UI controller for TRYX cooler displays (Panorama SE,
Panorama, Turris 620) on Linux. No Qt, no daemon, one binary.

Status: alpha. Works on a Panorama SE running the original cm01 firmware:
media upload with validation and conversion, file management, brightness,
a live metrics overlay, previews, and a terminal interface. Displays on the
newer KANALI firmware are detected but not driven yet.

## Install

Every release ships a static Linux binary and a Debian package, built by
`.github/workflows/release.yml`.

- **Tarball**: unpack it, copy `tryx` somewhere on your PATH, copy `udev/*.rules`
  to `/etc/udev/rules.d/`, run `sudo udevadm control --reload-rules`, and add
  yourself to the `dialout` and `plugdev` groups (log in again afterwards).
- **Debian/Ubuntu**: `sudo apt install ./tryx_*.deb` installs the binary, the
  udev rules, the man page, and shell completions, and reloads udev. Add
  yourself to `dialout` and `plugdev`.

At runtime `tryx` needs `ffmpeg` (with the libx264 encoder) and `adb` on the
PATH; the package recommends both. `tryx doctor` reports anything missing.

## Usage

```bash
tryx doctor                                  # tools, permissions, connected displays
tryx devices                                 # what is connected and how
tryx info                                    # identify the display
tryx media ls                                # files on the display and free space
tryx media check clip.mp4                    # what would change, and why
tryx media upload clip.mp4 --show            # validate, convert if needed, upload, play
tryx media preview clip.mp4 --at 5           # a frame exactly as the display gets it
tryx show clip.mp4 --play Loop               # play something already on the display
tryx display set --brightness 70
tryx metrics set --labels cpu-temp,gpu-temp,cpu-usage --badges cpu,gpu
tryx metrics push --interval 2               # send live readings until interrupted
tryx tui                                     # all of the above, interactively
tryx completions zsh > ~/.zfunc/_tryx
```

Every command takes `--json` for machine-readable output and `-v` to dump the
frames exchanged with the display. Exit codes: 2 usage, 3 device, 4
environment, 5 media rejected.

## Build from source

Enter the development shell (nix + direnv) and build:

```bash
direnv allow && cargo build
```

The shell provides `protoc`, `libusb`, and an `ffmpeg` with the `libx264`
encoder. Without nix, install those three yourself; `tryx doctor` reports
what is missing.

## Device access on Linux

Displays on the original `cm01` firmware expose a serial port owned by
`dialout` plus an ADB interface that the `71-tryx-legacy.rules` udev rule
opens to `plugdev`. Printer-class (KANALI) displays need the other two rules
or membership of the `lp` group.

## Attribution

The protobuf schemas and udev rules are copied from
[DXVSI/Tryx-Linux-GUI](https://github.com/DXVSI/Tryx-Linux-GUI) (MIT), whose
protocol and media pipeline this project reproduces. See
[THIRD_PARTY.md](THIRD_PARTY.md).

## Licence

MIT, see [LICENSE](LICENSE).
