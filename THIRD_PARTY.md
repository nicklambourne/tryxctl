# Third-party material

tryxctl is MIT licensed; see [LICENSE](LICENSE). It includes or builds on
material from the projects below. The MIT licence requires its copyright and
permission notice to travel with any copy, so each upstream notice is
reproduced in full under [Licence texts](#licence-texts). This file ships in
every release tarball and Debian package.

## DXVSI/Tryx-Linux-GUI (MIT)

<https://github.com/DXVSI/Tryx-Linux-GUI>, version 2.2.0.
Copyright (c) 2025 Fadli Arsani, copyright (c) 2026 DXVSI.

Copied unchanged, with attribution headers:

- `crates/tryx-proto/proto/wire-v1/*.proto`: the project-owned clean-room
  protobuf schemas for the KANALI wire protocol.
- `packaging/udev/70-tryx-access.rules` and
  `packaging/udev/99-tryx-printer.rules`: the two udev rules that grant seat
  access to the supported USB product IDs and stop CUPS from claiming them.
  The third rule, `71-tryx-legacy.rules`, is original to this project.

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
Copyright (c) 2026 AfroSamuraiX.

Its hardware notes on the legacy firmware informed several features: the fan
tachometer fields in the sysinfo reply, the `fanLCDSet` payload including the
vendor app's default smart-mode curve (copied as data into
`crates/tryx-legacy/src/commands.rs`), the `displayInSleep` and filter
semantics, the local-time offset on the sysinfo timestamp, and the
daemon-owns-the-port design.

## fadli0029/reed-tpse (MIT)

<https://github.com/fadli0029/reed-tpse>
Copyright (c) 2025 Fadli Arsani.

The original reverse engineering of the legacy `cm01` serial protocol and its
keepalive, from which the DXVSI code we ported descends.

## Licence texts

### DXVSI/Tryx-Linux-GUI

As published at tag `v2.2.0`.

```text
MIT License

Copyright (c) 2025 Fadli Arsani
Copyright (c) 2026 DXVSI

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

### AfroSamuraiX/panorama-manager

```text
MIT License

Copyright (c) 2026 AfroSamuraiX

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

### fadli0029/reed-tpse

```text
MIT License

Copyright (c) 2025 Fadli Arsani

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
