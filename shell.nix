{ pkgs ? import <nixpkgs> { } }:

# mkShellNoCC: on macOS a Nix C compiler shadows Apple's toolchain and breaks
# system linking. Host compilation uses the platform compiler; Nix supplies
# the ambient tools.
pkgs.mkShellNoCC {
  packages = with pkgs; [
    rustup # channel and components pinned by rust-toolchain.toml
    protobuf # protoc, consumed by prost-build
    pkg-config
    libusb1
    ffmpeg # must carry the libx264 encoder; `tryx doctor` verifies it
  ];
}
