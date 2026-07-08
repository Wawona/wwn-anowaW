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
  src = "${anowawSrc}/source/core";

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
    search_roots="''${CARGO_TARGET_DIR:-target} target"
    found=$(find $search_roots -name "libanowaw.so" 2>/dev/null | head -1)
    if [ -n "$found" ]; then
      echo "installing $found -> $out/lib/libanowaw.so"
      cp "$found" $out/lib/libanowaw.so
    else
      # Fall back to the static archive if cdylib was not produced.
      found=$(find $search_roots -name "libanowaw.a" 2>/dev/null | head -1)
      if [ -n "$found" ]; then
        echo "installing $found -> $out/lib/libanowaw.a"
        cp "$found" $out/lib/libanowaw.a
      else
        echo "ERROR: libanowaw.so/libanowaw.a not found" >&2
        find $search_roots -maxdepth 5 -type f -name "libanowaw*" 2>/dev/null >&2 || true
        exit 1
      fi
    fi
    if [ ! -f "$out/lib/libanowaw.so" ] && [ ! -f "$out/lib/libanowaw.a" ]; then
      echo "ERROR: native anowaW library was not installed into $out/lib" >&2
      exit 1
    fi
    cp ${anowawSrc}/source/include/anowaw.h $out/include/ 2>/dev/null || true

    # Ship the Kotlin/JNI shims so the Wawona Android build can stage them into
    # the app sourceSet + CMake sources.
    mkdir -p $out/share/anowaw/kotlin $out/share/anowaw/jni
    cp ${anowawSrc}/source/platform/android/kotlin/*.kt $out/share/anowaw/kotlin/ 2>/dev/null || true
    cp ${anowawSrc}/source/platform/android/jni/*.c $out/share/anowaw/jni/ 2>/dev/null || true
  '';
}
