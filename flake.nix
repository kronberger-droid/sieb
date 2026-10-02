{
  description = "sieb – dmenu-first Wayland picker";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # Toolchain on its own input so `nix flake update rust-overlay` gets the
    # newest stable without moving the nixpkgs pin. Same pattern as takt.
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    rust-overlay,
    ...
  }: let
    # Wayland only, so no darwin.
    forAllSystems = nixpkgs.lib.genAttrs ["x86_64-linux" "aarch64-linux"];
    pkgsFor = system:
      import nixpkgs {
        inherit system;
        overlays = [rust-overlay.overlays.default];
      };
    toolchainFor = pkgs:
      pkgs.rust-bin.stable.latest.default.override {
        extensions = ["rust-analyzer" "rust-src"];
      };
    version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
    # Linked C libraries: keyboard handling and font lookup. The Wayland
    # connection itself goes through wayland-client's pure Rust backend.
    nativeDepsFor = pkgs: [
      pkgs.libxkbcommon
      pkgs.fontconfig
    ];
  in {
    packages = forAllSystems (system: let
      pkgs = pkgsFor system;
    in {
      # nixpkgs' rustPlatform rather than the overlay toolchain, so the package
      # builds the same anywhere. Toolchain currency is a dev-shell concern.
      default = pkgs.rustPlatform.buildRustPackage {
        pname = "sieb";
        inherit version;
        src = self;
        cargoLock.lockFile = ./Cargo.lock;
        nativeBuildInputs = [pkgs.pkg-config];
        buildInputs = nativeDepsFor pkgs;
        # The example scripts double as the stock launcher and power menu,
        # so they ship with the binary for configs to point --script at.
        # nushell stays out of buildInputs on purpose: the scripts keep
        # `env nu` and run on whichever nu the user brings, rather than
        # dragging a second one into the closure.
        postInstall = ''
          mkdir -p $out/share/sieb
          cp -r examples $out/share/sieb/examples
        '';
        meta = {
          description = "dmenu-first Wayland picker, extended through scripts";
          license = pkgs.lib.licenses.mit;
          mainProgram = "sieb";
        };
      };
    });

    devShells = forAllSystems (system: let
      pkgs = pkgsFor system;
    in {
      default = pkgs.mkShell {
        nativeBuildInputs = [
          (toolchainFor pkgs)
          pkgs.pkg-config
        ];
        buildInputs = nativeDepsFor pkgs;
      };
    });
  };
}
