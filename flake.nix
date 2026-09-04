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
          # Tools tryx runs at runtime: ffmpeg with libx264 for media, adb for
          # legacy-firmware transfers. Wrapped onto PATH so the package works
          # on a bare system.
          runtime = [ pkgs.ffmpeg ] ++ lib.optionals stdenv.hostPlatform.isLinux [ pkgs.android-tools ];
          native = stdenv.buildPlatform.canExecute stdenv.hostPlatform;
        in
        rec {
          tryx = pkgs.rustPlatform.buildRustPackage {
            pname = "tryx";
            inherit version;
            src = lib.cleanSource ./.;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [ "-p" "tryx" ];

            nativeBuildInputs = [ pkgs.protobuf pkgs.pkg-config pkgs.installShellFiles pkgs.makeWrapper ];
            buildInputs = [ pkgs.libusb1 ];
            nativeCheckInputs = [ pkgs.ffmpeg ];

            # The man page and completions come from the binary itself.
            postInstall = lib.optionalString native ''
              $out/bin/tryx manpage > tryx.1
              installManPage tryx.1
              installShellCompletion --cmd tryx \
                --bash <($out/bin/tryx completions bash) \
                --zsh <($out/bin/tryx completions zsh) \
                --fish <($out/bin/tryx completions fish)
            '' + ''
              install -Dm644 -t $out/lib/udev/rules.d packaging/udev/*.rules
            '';
            postFixup = ''
              wrapProgram $out/bin/tryx --prefix PATH : ${lib.makeBinPath runtime}
            '';

            meta = {
              description = "Command-line controller for TRYX cooler displays";
              homepage = "https://github.com/nicklambourne/tryx-cli";
              license = lib.licenses.mit;
              mainProgram = "tryx";
              platforms = lib.platforms.unix;
            };
          };
          default = tryx;
        });

      devShells = forAll (pkgs: {
        default = import ./shell.nix { inherit pkgs; };
      });
    };
}
