#!/usr/bin/env python3
"""Patch-anchor verifier for the anowaW C ABI.

Mirrors wwn-weston's ``verify-weston-ios-patches.py`` style: it fails CI if the
C ABI drifts out of sync across the three surfaces that MUST agree —

  1. the Rust FFI (``core/src/ffi.rs``): ``#[no_mangle]`` exports,
  2. the public C header (``include/anowaw.h``): ``anowaw_*`` declarations,
  3. the ABI version constant (``core/src/lib.rs`` ``ANOWAW_ABI_VERSION``).

The Wawona app links these symbols in-process (ObjC ``WWNAnowaWRunner`` and JNI
``android_jni.c``). If an export is renamed/removed on one side but not the
other, the app fails to link at integration time — this check catches it early.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FFI = ROOT / "core" / "src" / "ffi.rs"
HEADER = ROOT / "include" / "anowaw.h"
LIB = ROOT / "core" / "src" / "lib.rs"

# The canonical set of exported entry points. Keep in sync intentionally: adding
# a new ABI function means adding it here AND bumping ANOWAW_ABI_VERSION.
REQUIRED_SYMBOLS = {
    "anowaw_abi_version",
    "anowaw_start",
    "anowaw_bridge_app",
    "anowaw_push_frame",
    "anowaw_poll_input",
    "anowaw_close_requested",
    "anowaw_close_app",
    "anowaw_dispatch",
    "anowaw_stop",
}


def fail(msg: str) -> None:
    print(f"ERROR: {msg}", file=sys.stderr)
    sys.exit(1)


def rust_exports(text: str) -> set[str]:
    # Match: pub [unsafe] extern "C" fn <name>(
    return set(
        re.findall(r'extern\s+"C"\s+fn\s+([A-Za-z0-9_]+)\s*\(', text)
    )


def header_decls(text: str) -> set[str]:
    # Match declarations/definitions of anowaw_* functions in the header.
    return set(re.findall(r'\b(anowaw_[A-Za-z0-9_]+)\s*\(', text))


def main() -> None:
    for p in (FFI, HEADER, LIB):
        if not p.exists():
            fail(f"missing required file: {p.relative_to(ROOT)}")

    ffi_text = FFI.read_text()
    header_text = HEADER.read_text()
    lib_text = LIB.read_text()

    exports = rust_exports(ffi_text)
    decls = header_decls(header_text)

    missing_rust = REQUIRED_SYMBOLS - exports
    if missing_rust:
        fail(f"Rust FFI missing exports: {sorted(missing_rust)}")

    missing_header = REQUIRED_SYMBOLS - decls
    if missing_header:
        fail(f"C header missing declarations: {sorted(missing_header)}")

    # The header must not declare functions the Rust side doesn't export.
    extra_header = decls - exports
    if extra_header:
        fail(f"C header declares symbols not exported by Rust: {sorted(extra_header)}")

    if "ANOWAW_ABI_VERSION" not in lib_text:
        fail("ANOWAW_ABI_VERSION constant not found in core/src/lib.rs")
    if "uint32_t anowaw_abi_version(void)" not in header_text:
        fail("anowaw_abi_version prototype missing/renamed in include/anowaw.h")

    print(f"OK: {len(REQUIRED_SYMBOLS)} ABI symbols consistent across ffi.rs + anowaw.h")


if __name__ == "__main__":
    main()
