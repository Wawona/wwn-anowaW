# Pure-source derivation for the anowaW bridge.
#
# Unlike wwn-waypipe (which fetches + patches an upstream crate), anowaW is
# Wawona's own code. This derivation assembles the Rust core crate together
# with the platform capture/inject shims into a single build tree consumed by
# the per-platform recipes (macos.nix, ios.nix, android.nix, ...).
#
# No compilation happens here — it only stages sources so the Nix hash of the
# source tree is independent of which platform recipe consumes it.
{ pkgs }:

pkgs.stdenvNoCC.mkDerivation {
  pname = "anowaw-src";
  version = "0.1.0";

  # Repo root is three levels up from dependencies/libs/anowaw/.
  src = ../../..;

  dontConfigure = true;
  dontBuild = true;
  dontFixup = true;

  installPhase = ''
    mkdir -p $out/source
    cp -r core $out/source/core
    cp -r platform $out/source/platform
    cp -r include $out/source/include 2>/dev/null || true
  '';

  meta = {
    description = "anowaW bridge source tree (Rust core + macOS/Android shims)";
    license = pkgs.lib.licenses.mit;
  };
}
