# anowaW bridge core for iOS/iPadOS.
#
# NOTE: on Apple *mobile* the bridge core builds (SHM Wayland client), but the
# capture + activity-launch pieces have no App-Store-safe equivalent (no
# per-window capture, no arbitrary activity launch), so Wawona does not enable
# the App Bridge on iOS. This recipe exists for link/instantiation parity and
# potential jailbroken/Developer use; see README "Scope".
{
  lib,
  pkgs,
  buildPackages,
  common,
  buildModule,
  simulator ? false,
  iosToolchain ? null,
  xcodeUtils ? iosToolchain,
}:

let
  anowawSrc = import ./anowaw-src.nix { inherit pkgs; };
  libwayland = buildModule.buildForIOS "libwayland" { inherit simulator; };
  rustTarget = if simulator then "aarch64-apple-ios-sim" else "aarch64-apple-ios";
  rustToolchain = pkgs.rust-bin.stable.latest.default.override {
    targets = [ rustTarget ];
  };
  rustPlatform = pkgs.makeRustPlatform {
    cargo = rustToolchain;
    rustc = rustToolchain;
  };
in
rustPlatform.buildRustPackage {
  pname = "anowaw";
  version = "0.1.0";
  src = anowawSrc;
  sourceRoot = "source/core";
  __noChroot = true;

  cargoLock = {
    lockFile = ./Cargo.lock;
  };

  buildNoDefaultFeatures = true;
  buildFeatures = [ ];

  nativeBuildInputs = [ xcodeUtils.findXcodeScript pkgs.pkg-config ];
  buildInputs = [ libwayland ];

  CARGO_BUILD_TARGET = rustTarget;
  doCheck = false;

  preConfigure = ''
    if [ -z "''${XCODE_APP:-}" ]; then
      XCODE_APP=$(${xcodeUtils.findXcodeScript}/bin/find-xcode || true)
      if [ -n "$XCODE_APP" ]; then
        export DEVELOPER_DIR="$XCODE_APP/Contents/Developer"
        export SDKROOT="$DEVELOPER_DIR/Platforms/${if simulator then "iPhoneSimulator" else "iPhoneOS"}.platform/Developer/SDKs/${if simulator then "iPhoneSimulator" else "iPhoneOS"}.sdk"
      fi
    fi
    export PKG_CONFIG_PATH="${libwayland}/lib/pkgconfig:$PKG_CONFIG_PATH"
    export RUSTFLAGS="-A warnings $RUSTFLAGS"
  '';

  postInstall = ''
    mkdir -p $out/lib $out/include
    found=$(find target -name "libanowaw.a" 2>/dev/null | head -1)
    if [ -n "$found" ]; then
      cp "$found" $out/lib/libanowaw.a
    else
      echo "ERROR: libanowaw.a not found" >&2
      exit 1
    fi
    cp ../../include/anowaw.h $out/include/ 2>/dev/null || true
  '';
}
