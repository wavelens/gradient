/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

fn main() -> std::io::Result<()> {
    tonic_prost_build::configure()
        .build_transport(false)
        .compile_protos(&["proto/nix_remote.proto"], &["proto"])
}
