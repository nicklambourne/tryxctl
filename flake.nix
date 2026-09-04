{
  description = "Command-line controller for TRYX cooler displays";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      # The tool drives Linux USB and serial devices; the dev shell also
      # works on macOS for the media and protocol code.
      linux = [ "x86_64-linux" "aarch64-linux" ];
      systems = linux ++ [ "x86_64-darwin" "aarch64-darwin" ];
      forEach = list: f: nixpkgs.lib.genAttrs list (system: f nixpkgs.legacyPackages.${system});
      forAll = forEach systems;
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
    in
    {
      packages = forEach linux (pkgs:
        let
          inherit (pkgs) lib stdenv;
          # Tools tryxctl runs at runtime: ffmpeg with libx264 for media, adb for
          # legacy-firmware transfers. Wrapped onto PATH so the package works
          # on a bare system.
          runtime = [ pkgs.ffmpeg ] ++ lib.optionals stdenv.hostPlatform.isLinux [ pkgs.android-tools ];
          native = stdenv.buildPlatform.canExecute stdenv.hostPlatform;
        in
        rec {
          tryxctl = pkgs.rustPlatform.buildRustPackage {
            pname = "tryxctl";
            inherit version;
            src = lib.cleanSource ./.;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [ "-p" "tryxctl" ];

            nativeBuildInputs = [ pkgs.protobuf pkgs.pkg-config pkgs.installShellFiles pkgs.makeWrapper ];
            buildInputs = [ pkgs.libusb1 ];
            nativeCheckInputs = [ pkgs.ffmpeg ];

            # The man page and completions come from the binary itself.
            postInstall = lib.optionalString native ''
              $out/bin/tryxctl manpage > tryxctl.1
              installManPage tryxctl.1
              installShellCompletion --cmd tryxctl \
                --bash <($out/bin/tryxctl completions bash) \
                --zsh <($out/bin/tryxctl completions zsh) \
                --fish <($out/bin/tryxctl completions fish)
            '' + ''
              install -Dm644 -t $out/lib/udev/rules.d packaging/udev/*.rules
            '';
            postFixup = ''
              wrapProgram $out/bin/tryxctl --prefix PATH : ${lib.makeBinPath runtime}
            '';

            meta = {
              description = "Command-line controller for TRYX cooler displays";
              homepage = "https://github.com/nicklambourne/tryxctl";
              license = lib.licenses.mit;
              mainProgram = "tryxctl";
              platforms = lib.platforms.unix;
            };
          };
          default = tryxctl;
        });

      devShells = forAll (pkgs: {
        default = import ./shell.nix { inherit pkgs; };
      });
    };
}
