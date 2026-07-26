{
  lib,
  montserrat,
  pkg-config,
  rustPlatform,
  udev,
}:
rustPlatform.buildRustPackage {
  pname = "scd";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.lock
      ../Cargo.toml
      ../src
    ];
  };

  cargoLock = {
    lockFile = ../Cargo.lock;
    outputHashes."blit-0.1.0" = "sha256-K3ZvWTqrkGw06SRa59w0etwhdcW4rpqRedW9l3wcP9w=";
  };
  buildFeatures = ["osk"];

  nativeBuildInputs = [pkg-config];
  buildInputs = [udev];

  RUSTC_BOOTSTRAP = "1";
  SCD_OSK_FONT = "${montserrat}/share/fonts/ttf/Montserrat-SemiBold.ttf";

  meta = {
    description = "Steam Controller userspace daemon";
    mainProgram = "scd";
    platforms = lib.platforms.linux;
  };
}
