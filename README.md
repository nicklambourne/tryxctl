# tryxctl

Command-line and terminal-UI controller for TRYX cooler displays (Panorama SE,
Panorama, Turris 620) on Linux. No Qt, no daemon, one binary.

Status: alpha. Verified on a Panorama SE running the original cm01 firmware:
media upload with validation and conversion, file management, brightness,
filters, a live metrics overlay, fan readings, previews, and a terminal
interface. Displays on the newer KANALI firmware (printer-class USB) are
driven through the same commands: catalog, upload, removal, brightness,
media selection, and the overlay. That backend is a port of the upstream
protocol tested against a scripted fake device, not yet against hardware.

## Install

Every release ships static Linux binaries and Debian packages for x86_64
and aarch64, built by `.github/workflows/release.yml`.

- **Tarball**: unpack it, copy `tryxctl` somewhere on your PATH, copy `udev/*.rules`
  to `/etc/udev/rules.d/`, run `sudo udevadm control --reload-rules`, and add
  yourself to the `dialout` and `plugdev` groups (log in again afterwards).
- **Debian/Ubuntu**: `sudo apt install ./tryxctl_*.deb` installs the binary, the
  udev rules, the man page, and shell completions, and reloads udev. Add
  yourself to `dialout` and `plugdev`.

- **Nix**: `nix profile install github:nicklambourne/tryxctl` (or
  `nix build` in a checkout). The package wraps `ffmpeg` and `adb` onto the
  binary's PATH and ships the udev rules under `lib/udev/rules.d` for
  `services.udev.packages` on NixOS.

At runtime `tryxctl` needs `ffmpeg` (with the libx264 encoder) and, for the
legacy firmware, `adb` on the PATH; the Debian package recommends both.
`tryxctl doctor` reports anything missing.

## Usage

```bash
tryxctl doctor                                  # tools, permissions, connected displays
tryxctl devices                                 # what is connected and how
tryxctl info                                    # identify the display
tryxctl media ls                                # files on the display and free space
tryxctl media check clip.mp4                    # what would change, and why
tryxctl media check --target kanali-turris a.png  # ...for a display that is not connected
tryxctl media upload clip.mp4 --show            # validate, convert if needed, upload, play
tryxctl media upload long.mp4 --trim 30-90      # only part of a video
tryxctl media preview clip.mp4 --at 5           # a frame exactly as the display gets it
tryxctl media export clip.mp4 -o backup.mp4     # a copy of a file on the display
tryxctl media replace clip.mp4 new.mp4          # swap the file under its name, no gap
tryxctl op ls                                   # recent transfers; a failed one keeps its encode
tryxctl op retry 6aa17661ce3f                   # send it again without re-encoding
tryxctl show clip.mp4 --play Loop               # play something already on the display
tryxctl show preset:3                           # one of the six built-in animations
tryxctl display get                             # what the panel is set to
tryxctl display set --brightness 70
tryxctl display set --mode split --waterfall on --rotate 180
tryxctl metrics set --labels cpu-temp,gpu-temp,cpu-usage --badges cpu,gpu
tryxctl metrics set --area right --labels gpu-usage   # the right half in split mode
tryxctl daemon install                          # the daemon: keepalive, live metrics, and every
                                             # command below routes through it (systemd user service)
tryxctl daemon status                           # what it knows: device, fans, pushes, last error
tryxctl fans --watch 5                          # LCD fan and pump RPM from the display
tryxctl fans --lcd-speed 40                     # fixed display-block fan speed
tryxctl fans --lcd-speed auto                   # back to the firmware's curve
tryxctl metrics status --watch 2                # host readings every 2 s
tryxctl display set --filter smoke --filter-opacity 60
tryxctl display set --sleep on                  # let the panel sleep with the host
tryxctl display reboot
tryxctl tui                                     # all of the above, interactively:
                                                # devices, library, overlay, display, transfers
tryxctl completions zsh > ~/.zfunc/_tryxctl
```

The panel goes dark about a minute after the host stops talking to it, so
`tryxctl daemon install` is the normal way to run things: the daemon owns the
serial port, keeps the panel awake with live readings, restores the saved
screen on start, answers the other commands over a socket so they never
compete for the port, reopens the link when the display reboots or is
replugged, and applies the screen again after the host wakes from sleep. The legacy firmware answers no queries, so `display get` reports
what was last applied; a KANALI display is read back for real. Without a daemon every command opens the port itself;
`--direct` forces that. After adding yourself to `dialout`, restart your
systemd user manager (log out fully, or `systemctl --user exit` and log in
again) or the service will not see the new group.

The interface previews media inline: a short looping clip of the selected
file on the display, and the file being uploaded as the display will get
it (four seconds at six frames a second; a GIF animates, a still stays).
`p` plays the selected file or the wizard's file in real time instead:
ffmpeg streams it at its own pace, frames the redraw does not reach are
dropped, and the pane's title shows the rate achieved. Kitty-protocol
terminals get real pixels; everything else gets half-blocks. Sixel and
iTerm2 terminals fall back to half-blocks for playback too, since they
would re-send every frame in full. It uses
kitty graphics, iTerm2 images, or Sixel when the terminal offers them, and
coloured half-blocks otherwise; `TRYXCTL_GRAPHICS=kitty|iterm2|sixel|halfblocks`
forces one, which is also how to get pictures inside tmux with
`allow-passthrough on`. A file on the display whose index sits at the end
of the container (no faststart) is pulled once to make its thumbnail,
which is then cached.

Every command takes `--json` for machine-readable output, `-q` for results
and errors only, `--no-color`, and `-v` to dump the frames exchanged with
the display. With several displays attached, `--tty`
picks a legacy serial port and `--device` a KANALI USB id (both listed by
`tryxctl devices`). Exit codes: 2 usage, 3 device, 4 environment, 5 media
rejected.

### Firmware differences

| | Legacy cm01 (serial + ADB) | KANALI (printer-class USB) |
|---|---|---|
| Media | MP4, 1920×960 | raw H.264 at 2240×1080 (Panorama) or 1280×720 in the MXHD header (Turris 620); still images become loops |
| Names | as uploaded | suffixed `.h264_2240x1080` / `.h264_1280x720` |
| Overlay | up to 3 labels, incl. voltages and disk/motherboard temperature | up to 3 labels, incl. CPU/GPU power; no voltages |
| Keepalive | sysinfo push every few seconds | ping and overlay lease every 2 s |
| Filters, sleep, fans, reboot | yes | not in the protocol |
| Readback | none; `display get` shows the last applied state | `display get` reads the device |
| Pump RPM | only on models advertising `Turbo Pump`; the Panorama SE has no pump tachometer | not in the protocol |
| Storage | `df` over ADB | none; the catalog lists sizes |

## Build from source

Enter the development shell (nix + direnv, or `nix develop`) and build:

```bash
direnv allow && cargo build
```

The shell provides `protoc`, `libusb`, and an `ffmpeg` with the `libx264`
encoder. Without nix, install those three yourself; `tryxctl doctor` reports
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
