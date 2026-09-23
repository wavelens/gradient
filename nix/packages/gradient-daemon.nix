/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib
, craneLib
, writeText
}:
let
  daemonSrc = ../../daemon;
  src = lib.fileset.toSource {
    root = daemonSrc;
    fileset = lib.fileset.unions [
      (craneLib.fileset.commonCargoSources daemonSrc)
      (daemonSrc + "/tests/fixtures")
    ];
  };

  # harmonia (git dep) ships crates whose Cargo.toml points at a README.md
  # outside the crate dir; strip the readme key so vendoring works.
  cargoVendorDir = craneLib.vendorCargoDeps {
    inherit src;
    overrideVendorGitCheckout = _ps: drv:
      drv.overrideAttrs (old: {
        postPatch = (old.postPatch or "") + ''
          find . -name "Cargo.toml" | xargs sed -i '/^readme\s*=/d'
        '';
      });
  };

  dummyrs = writeText "dummy.rs" ''
    #![allow(clippy::all)]
    #![allow(dead_code)]
    pub fn main() {}
  '';

  commonArgs = {
    inherit src cargoVendorDir;
    pname = "gradient-daemon";
    version = "1.3.0";
    strictDeps = true;
    cargoExtraArgs = "--locked --features mock";
    CARGO_INCREMENTAL = "0";
  };

  cargoArtifacts = craneLib.buildDepsOnly (commonArgs // { inherit dummyrs; });
in
craneLib.buildPackage (commonArgs // {
  inherit cargoArtifacts;
  doCheck = false;

  passthru.clippy = craneLib.cargoClippy (commonArgs // {
    inherit cargoArtifacts;
    cargoClippyExtraArgs = "--all-targets -- -D warnings";
  });

  passthru.tests = craneLib.cargoNextest (commonArgs // {
    inherit cargoArtifacts;
  });

  meta = {
    description = "Nix daemon protocol server with a mock store backend, for Gradient's scheduler tests";
    homepage = "https://github.com/wavelens/gradient";
    license = lib.licenses.agpl3Only;
    mainProgram = "gradient-daemon";
  };
})
