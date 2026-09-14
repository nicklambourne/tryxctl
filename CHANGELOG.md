# Changelog

Notable changes in each release. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). tryxctl is alpha,
so any release before 1.0 may change behaviour.

## [Unreleased]

### Added

- A disclaimer, a key reference for the interface, troubleshooting, the files
  the tool writes, and uninstall steps in the README, with screenshots.
- A contributing guide and this changelog.
- `metrics set --celsius`, to go back from `--fahrenheit`.
- End-to-end tests against fake displays of both firmware families, a fake
  adb, and the daemon, with property tests for the codecs and parsers.

### Changed

- `metrics set` keeps the temperature unit it was last given instead of
  resetting it to Celsius.

### Fixed

- With the daemon running, a command given `--tty` or `--device` for another
  display acted on the daemon's display instead.
- adb could pick another Android device, such as a phone, when the display's
  own transport was missing, and media commands on a port with no display
  behind it guessed at one.
- Two transfers begun within the same second by one process, as the interface
  can, shared an id and overwrote each other's journal record.
- `op retry` with an empty or ambiguous id retried the oldest match; it now
  needs a whole id or a unique prefix.
- `media export -o DIR` failed instead of writing into the directory.
- Exports from the interface, and the copies behind previews and playback,
  were not checked for completeness.
- Clearing kept encodes in the interface printed over the screen.
- `fans --lcd-speed` opened the display before checking its value.
- `show --play` was case-sensitive, unlike every other choice.
- The daemon served one client at a time, so one that stalled held up every
  other command.
- `daemon status` showed the old media while a preset played, and lost the
  reason when a KANALI display went away.
- A KANALI reply that arrived in the same read as stray bytes was discarded
  with them, and the command timed out.
- Long file names could end in a dash or a dot after shortening.
- `THIRD_PARTY.md` reproduces the upstream MIT notices in full, and the files
  copied from upstream carry their copyright lines.

## [0.3.0] - 2026-09-13

### Added

- Previews in the interface play a short looping clip instead of a single
  frame.
- `p` plays the selected file in real time in the preview pane, both for files
  on the display and for the file in the upload wizard.
- On kitty-protocol terminals, playback frames travel through shared memory on
  a local kitty or Ghostty and as zlib-compressed pixels elsewhere, with a
  leaner picture over SSH. `TRYXCTL_KITTY_TRANSFER` overrides the choice.

### Fixed

- `daemon status` no longer shows the display as unknown straight after the
  daemon restarts; the handshake is retried when its first reply carries no
  identity.

## [0.2.0] - 2026-09-10

### Changed

- Renamed from `tryx` to `tryxctl`: the binary, the systemd user unit
  `tryxctl.service`, and the socket and state directories. The udev rules keep
  their file names.
- `--quiet` and `--no-color` are global options.

### Added

- `show preset:N` plays one of the six built-in animations.
- `display set --mode`, `--waterfall`, and `--rotate`, and
  `metrics set --area right` for the second half in split mode.
- `display get` reads a KANALI display's settings back and reports what was
  last applied on the legacy firmware.
- The daemon starts without a display, reconnects when the display reboots or
  is replugged, and applies the screen again after the host resumes from
  suspend.
- `fans --lcd-speed auto` hands the display-block fan back to the firmware's
  curve. Pump RPM is reported only by models that have a pump tachometer.
- `media upload --trim`, `media export`, and `media replace`.
- A transfer journal: `op ls` lists transfers, `op retry` resends a failed one
  from its kept encode without running ffmpeg again, and `op clear` tidies up.
- `metrics status --watch`.
- Interface: devices and operations tabs, an upload wizard showing the file's
  findings and plan for a chosen fit, rotation, and zoom, cancelling an
  encode, the built-in animations and export in the library, and split
  screen, waterfall, rotation, and readback in the display tab.
- Inline previews through the terminal's graphics protocol: kitty, iTerm2,
  Sixel, or half-blocks. `TRYXCTL_GRAPHICS` forces one.

### Fixed

- A closed output pipe, as in `tryxctl media ls | head`, ends the process
  quietly instead of panicking.

## [0.1.0] - 2026-09-04

First release, under the name `tryx`.

### Added

- The legacy cm01 firmware backend: the serial protocol, media transfer over
  ADB, and `doctor`, `devices`, `info`, and `show`, with `--verbose` to dump
  every frame.
- The media engine: `media check`, `convert`, `upload`, `rm`, and `preview`,
  which validate media and convert it to what the display accepts.
- A live metrics overlay pushed from the host.
- A daemon that owns the port, keeps the panel awake, restores the screen, and
  answers the other commands over a socket.
- Fan readings and speed, filters, sleep, and a date and time label.
- The KANALI native USB backend, with raw H.264 media for the Panorama and the
  MXHD format for the Turris 620, tested against a scripted fake device.
- A terminal interface with library, overlay, and display tabs.
- Static tarballs and Debian packages for x86_64 and aarch64, shell
  completions, a man page, and a nix flake.

[Unreleased]: https://github.com/nicklambourne/tryxctl/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/nicklambourne/tryxctl/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/nicklambourne/tryxctl/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/nicklambourne/tryxctl/releases/tag/v0.1.0
