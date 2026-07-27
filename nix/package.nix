{
  lib,
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
      ../core
      ../ctl
      ../daemon
      ../osk
    ];
  };

  cargoLock = {
    lockFile = ../Cargo.lock;
    outputHashes."blit-0.1.0" = "sha256-YAac7/WoiC/xNGz+KIOOaqEH6240ml7mvJPIJU6Ew9k=";
  };
  nativeBuildInputs = [pkg-config];
  buildInputs = [udev];

  RUSTC_BOOTSTRAP = "1";
  meta = {
    description = "Steam Controller userspace daemon";
    mainProgram = "scd";
    platforms = lib.platforms.linux;
  };
}
