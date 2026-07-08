# anowaW bridge core for macOS — builds the Rust core as a static lib
# (libanowaw.a) exporting the C ABI (anowaw_start, anowaw_bridge_app, …) plus
# the ScreenCaptureKit/CGEvent shim object. Linked in-process by the Wawona
# macOS app (WWNAnowaWRunner), the same way libwaypipe.a is linked today.
#
# macOS target: Developer ID (non-App-Store), consistent with the SIP-gated
# wwn-iland "Mode B" desktop replacement. ScreenCaptureKit + CGEvent injection
# are permitted here.
{
  lib,
  pkgs,
  common,
  buildModule,
  xcodeUtils,
}:

let
  anowawSrc = import ./anowaw-src.nix { inherit pkgs; };
  libwayland = buildModule.buildForMacOS "libwayland" { };
  cargoTarget = pkgs.stdenv.hostPlatform.rust.rustcTarget;
in
pkgs.rustPlatform.buildRustPackage {
  pname = "anowaw";
  version = "0.1.0";
  src = "${anowawSrc}/source/core";

  cargoLock = {
    lockFile = ./Cargo.lock;
  };

  # Bridge core only needs SHM on macOS today; the IOSurface zero-copy path is
  # gated behind the "dmabuf" feature and enabled once the Metal import lands.
  buildNoDefaultFeatures = true;
  buildFeatures = [ ];

  nativeBuildInputs = [
    pkgs.pkg-config
    xcodeUtils.findXcodeScript
  ];

  buildInputs = [
    libwayland
    pkgs.libiconv
  ];

  CARGO_BUILD_TARGET = cargoTarget;

  preConfigure = ''
    MACOS_SDK=$(xcrun --sdk macosx --show-sdk-path 2>/dev/null || true)
    if [ ! -d "$MACOS_SDK" ]; then
      MACOS_SDK="/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk"
    fi
    export SDKROOT="$MACOS_SDK"
    export MACOSX_DEPLOYMENT_TARGET="26.0"
    export PKG_CONFIG_PATH="${libwayland}/lib/pkgconfig:$PKG_CONFIG_PATH"
    export RUSTFLAGS="-A warnings $RUSTFLAGS"
  '';

  # Compile the ScreenCaptureKit + CGEvent shim and bundle it next to the lib so
  # the Wawona app links both. The shim drives the Rust core through the C ABI.
  postBuild = ''
    SHIM=${anowawSrc}/source/platform/macos/AnowawMacBridge.m
    if [ -f "$SHIM" ]; then
      clang -c "$SHIM" \
        -fobjc-arc -fPIC \
        -I${anowawSrc}/source/include \
        -isysroot "$SDKROOT" -mmacosx-version-min=26.0 \
        -o anowaw_mac_shim.o || echo "warning: shim compile deferred (SDK frameworks)"
    fi
  '';

  postInstall = ''
    mkdir -p $out/lib $out/include
    # Locate the static lib across host/cross target dirs.
    found=""
    for cand in \
      "target/${cargoTarget}/release/libanowaw.a" \
      "target/release/libanowaw.a"; do
      if [ -f "$cand" ]; then found="$cand"; break; fi
    done
    if [ -n "$found" ]; then
      cp "$found" $out/lib/libanowaw.a
    else
      echo "ERROR: libanowaw.a not found" >&2
      find target -name "libanowaw.a" 2>/dev/null || true
      exit 1
    fi
    [ -f anowaw_mac_shim.o ] && cp anowaw_mac_shim.o $out/lib/ || true
    cp ${anowawSrc}/source/include/anowaw.h $out/include/ 2>/dev/null || true
    # Also export the ObjC capture/inject shim header so downstream consumers
    # (the Wawona macOS app's WWNAnowaWController) can #import "AnowawMacBridge.h"
    # and link the shim object above. Without this the consumer falls back to its
    # no-op stub (guarded by __has_include).
    cp ${anowawSrc}/source/platform/macos/AnowawMacBridge.h $out/include/ 2>/dev/null || true
  '';

  doCheck = false;
}
