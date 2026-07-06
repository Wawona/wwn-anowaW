# anowaW bridge core for Android — builds the Rust core as a shared lib
# (libanowaw.so) exporting the C ABI, loaded in-process by the Wawona Android
# app via JNI (android_jni.c), the same way libwaypipe_bin.so is used.
#
# The Kotlin/Java side (VirtualDisplay capture, MediaProjection consent,
# Shizuku power mode, InputManager injection) lives in the Wawona app under
# platform/android/ and drives this core over the C ABI.
{
  lib,
  pkgs,
  buildPackages,
  common,
  buildModule,
  androidToolchain ? (import ../../toolchains/android.nix { inherit lib pkgs; }),
  ...
}:

let
  anowawSrc = import ./anowaw-src.nix { inherit pkgs; };
  libwayland = buildModule.buildForAndroid "libwayland" { };

  rustToolchain = pkgs.rust-bin.stable.latest.default.override {
    targets = [ "aarch64-linux-android" ];
  };
  rustPlatform = pkgs.makeRustPlatform {
    cargo = rustToolchain;
    rustc = rustToolchain;
  };

  androidLinkerWrapper = pkgs.writeShellScript "android-linker-wrapper" ''
    exec ${androidToolchain.androidCC} "$@"
  '';
in
rustPlatform.buildRustPackage {
  pname = "anowaw";
  version = "0.1.0";
  src = anowawSrc;
  sourceRoot = "source/core";

  cargoLock = {
    lockFile = ./Cargo.lock;
  };

  # SHM path on Android for v1; AHardwareBuffer dmabuf import is gated behind
  # "dmabuf" and enabled once the AHardwareBuffer→dmabuf export path lands.
  buildNoDefaultFeatures = true;
  buildFeatures = [ ];

  nativeBuildInputs = with buildPackages; [ pkg-config ];
  buildInputs = [ libwayland ];

  CARGO_BUILD_TARGET = "aarch64-linux-android";
  CC_aarch64_linux_android = "${androidLinkerWrapper}";
  CXX_aarch64_linux_android = androidToolchain.androidCXX;
  AR_aarch64_linux_android = androidToolchain.androidAR;
  CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = "${androidLinkerWrapper}";

  cargoBuildFlags = [ "--target" "aarch64-linux-android" ];
  doCheck = false;
  dontFixup = true;

  preConfigure = ''
    export PKG_CONFIG_PATH="${libwayland}/lib/pkgconfig:$PKG_CONFIG_PATH"
    export PKG_CONFIG_ALLOW_CROSS=1
    export RUSTFLAGS="-A warnings $RUSTFLAGS"
  '';

  postInstall = ''
    mkdir -p $out/lib $out/include
    found=$(find target -name "libanowaw.so" 2>/dev/null | head -1)
    if [ -n "$found" ]; then
      cp "$found" $out/lib/libanowaw.so
    else
      # Fall back to the static archive if cdylib was not produced.
      found=$(find target -name "libanowaw.a" 2>/dev/null | head -1)
      [ -n "$found" ] && cp "$found" $out/lib/libanowaw.a
    fi
    cp ../../include/anowaw.h $out/include/ 2>/dev/null || true

    # Ship the Kotlin/JNI shims so the Wawona Android build can stage them into
    # the app sourceSet + CMake sources (sourceRoot is source/core, so the repo
    # root is two levels up).
    mkdir -p $out/share/anowaw/kotlin $out/share/anowaw/jni
    cp ../../platform/android/kotlin/*.kt $out/share/anowaw/kotlin/ 2>/dev/null || true
    cp ../../platform/android/jni/*.c $out/share/anowaw/jni/ 2>/dev/null || true
  '';
}
