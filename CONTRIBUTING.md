# Contributing to tryxctl

Thanks for helping. Bug reports, hardware reports, documentation fixes, and
code are all welcome. This guide covers setting up a development environment,
the checks a change has to pass, and how the code is organised.

## Reporting a problem

Open an issue with:

- what you ran and what happened, with the full error text;
- the output of `tryxctl doctor` and `tryxctl --version`;
- the display model and firmware, from `tryxctl info`.

For a problem talking to the display, run the failing command again with `-v`,
which dumps every frame exchanged with it, and attach the output. **Remove
your display's serial number** from anything you paste: `tryxctl devices`,
`tryxctl info`, and the `-v` dumps all include it.

### Hardware reports

tryxctl has been verified on a Panorama SE running the original cm01
firmware. The KANALI backend, for the newer printer-class firmware, has only
run against a scripted fake device, so a report from anyone with a KANALI
Panorama, Panorama SE, or Turris 620 is especially useful, even when
everything works. Say which of these you tried and what each did:
`devices`, `info`, `media ls`, `media upload`, `media rm`, `show`,
`display set --brightness`, `metrics set`, and the daemon.

### Security issues

Please report anything with security implications privately, by email to
dev@ndl.au, rather than in a public issue.

## Development environment

The repository pins the stable Rust toolchain, with rustfmt and clippy, in
`rust-toolchain.toml`. Building also needs `protoc`, `pkg-config` and libusb,
and the tests and the tool itself need an `ffmpeg` with the `libx264` encoder.
`adb` is only needed to transfer media to a legacy-firmware display.

**With nix and direnv**, `direnv allow` in a checkout enters a shell with all
of them; `nix develop` does the same without direnv. Settings that only make
sense on your machine, such as a build directory on another disk, go in
`.envrc.local`, which `.envrc` loads when it exists and git ignores:

```bash
export CARGO_TARGET_DIR=/path/to/fast/disk/tryxctl-target
```

**Without nix**, install the dependencies yourself. On Debian or Ubuntu:

```bash
sudo apt-get install ffmpeg libusb-1.0-0-dev pkg-config protobuf-compiler adb
```

Then check the environment with `cargo run -- doctor`.

The tool itself runs only on Linux, which is what the displays' udev rules,
serial ports, and USB access need. The code builds and most tests run on macOS
too, which is enough for work that does not touch a device.

## Checks

CI runs these on every pull request, and they need to pass before a change
is merged:

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo run --quiet -- doctor
nix build .#tryxctl
```

The tests also run on aarch64, and a coverage job fails when line coverage
drops below its floor; `cargo llvm-cov --workspace --all-targets` measures it
locally. CI sets `TRYXCTL_REQUIRE_FFMPEG=1`, which turns a test that would skip
for want of ffmpeg into a failure.

With no display attached, `doctor` still passes, with warnings. It exits with
code 4 only when something it needs is missing, such as `ffmpeg` or its
`libx264` encoder. The nix package builds on Linux only.

### Testing without hardware

Nothing in the test suite needs a display, and nothing in it touches the
host's: every run of the binary happens in a sandbox with its own XDG
directories, temporary directory, `PATH`, and USB device tree.

- `crates/tryx-testkit` holds the doubles. `FakeCm01` answers the legacy
  protocol on a pseudo-terminal and records every request; `FakeAdb` is a
  shell script serving the display's media directory; `FakeKanali` speaks the
  KANALI protocol on a socket; `Sandbox` builds the environment around them.
  `TRYX_TESTKIT_KEEP=1` keeps sandboxes after a test, for a look inside.
- `TRYXCTL_SYSFS_USB_DEVICES` and `TRYXCTL_DEV_DIR` point discovery at a
  device tree in place of `/sys/bus/usb/devices` and `/dev`. While they are
  set, KANALI displays come from that tree too, reached over a socket in their
  device directory instead of USB.
- `crates/tryxctl/tests/` runs the built binary against the fakes: the command
  line, the daemon, the interface on a pseudo-terminal, and both firmware
  families. The interface's device thread is tested in process in
  `src/tui/worker.rs`, each test in its own sandboxed process.
- The KANALI session is also tested below the command line, against a
  scripted device in `crates/tryx-kanali/tests/fake_device.rs`. Extend both
  fakes when you change the protocol.
- The legacy protocol tests decode frames captured from a real Panorama SE,
  kept as fixtures next to the codec in `crates/tryx-legacy/src/frame.rs`.
- Media checks and plans are tested against real ffprobe output in
  `crates/tryx-media/tests/fixtures/`, and the codecs and parsers with
  property tests in each crate's `tests/properties.rs`.

Linux runs everything. Opening a pseudo-terminal as a serial port and the
daemon work only there, so those tests are compiled on Linux alone; the rest
run on macOS too.

If you change behaviour that only a display can confirm, say in the pull
request what you verified on hardware, and on which model and firmware.

## How the code is organised

| Crate | Responsibility |
|---|---|
| `crates/tryxctl` | The binary: the CLI, the daemon and its socket, and the terminal interface in `src/tui/` |
| `crates/tryx-device` | Discovery of both firmware families and the product profiles |
| `crates/tryx-legacy` | The legacy cm01 firmware: byte-stuffed JSON commands over serial, and media over ADB |
| `crates/tryx-kanali` | The KANALI firmware: USB transport, session, and the overlay layout |
| `crates/tryx-proto` | The KANALI frame codec and its protobuf messages |
| `crates/tryx-media` | Probing, validation, conversion plans, encoding, and the MXHD container |
| `crates/tryx-monitor` | Host metrics for the overlay: CPU, GPU, memory, and disk |
| `crates/tryx-testkit` | Test doubles: fake displays of both firmware families, a fake adb, and sandboxes; never published |

## Making a change

- Keep a pull request to one change, and add or update tests with it.
- Update the README when a command, option, or key changes, and add a line
  under `Unreleased` in `CHANGELOG.md` for anything a user would notice.
- Commit messages start with the area and an imperative summary, such as
  `TUI: real-time playback in the preview pane` or
  `Daemon: retry the handshake once when its reply carries no identity`. The
  body explains why the change is needed, not just what it does.

### Code from elsewhere

Only copy code, data, or schemas under a licence compatible with MIT, and
record where each came from in `THIRD_PARTY.md`, with the upstream copyright
and licence notice in full. The GPL-3.0 projects listed there are behavioural
references only: do not copy from them.

## Releases

For maintainers:

1. Bump `version` under `[workspace.package]` in `Cargo.toml`, then run
   `cargo update --workspace` so `Cargo.lock` follows. Do this before
   tagging; the release assets take their names from it.
2. Move the `Unreleased` entries in `CHANGELOG.md` under the new version and
   date.
3. Commit, wait for CI to pass, then push an annotated tag such as `v0.4.0`.
   The release workflow builds the tarballs and Debian packages for both
   architectures and publishes them.

## Licence

By contributing, you agree that your contributions are licensed under the MIT
licence that covers the project; see [LICENSE](LICENSE).
