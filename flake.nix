{
  description = "Steam Controller userspace daemon";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = {
    self,
    nixpkgs,
  }: let
    systems = [
      "aarch64-linux"
      "x86_64-linux"
    ];
    forAllSystems = nixpkgs.lib.genAttrs systems;
  in {
    packages = forAllSystems (system: let
      pkgs = nixpkgs.legacyPackages.${system};
      scd = pkgs.callPackage ./nix/package.nix {};
    in {
      inherit scd;
      default = scd;
    });

    checks = forAllSystems (system: {
      package = self.packages.${system}.default;
    });

    devShells = forAllSystems (system: let
      pkgs = nixpkgs.legacyPackages.${system};
    in {
      default = pkgs.mkShell {
        packages = with pkgs; [
          cargo
          clippy
          pkg-config
          rust-analyzer
          rustc
          rustfmt
        ];
        buildInputs = [pkgs.udev];
        RUSTC_BOOTSTRAP = "1";
      };
    });

    nixosModules = {
      default = import ./nix/module.nix;
      scd = self.nixosModules.default;
    };
  };
}
