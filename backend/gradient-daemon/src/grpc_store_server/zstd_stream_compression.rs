/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::io;
use zstd::stream::raw::{Decoder, Encoder, InBuffer, Operation as _, OutBuffer};
use zstd::zstd_safe::{CCtx, DCtx};

pub struct ZstdStreamCompressor(Encoder<'static>);

impl ZstdStreamCompressor {
    pub fn new() -> io::Result<Self> {
        Encoder::new(zstd::DEFAULT_COMPRESSION_LEVEL).map(Self)
    }

    pub fn compress(&mut self, data: &[u8]) -> io::Result<Vec<u8>> {
        let mut piece = Vec::new();
        let mut buffer = vec![0; CCtx::out_size()];
        let mut input = InBuffer::around(data);
        while input.pos() < data.len() {
            let mut output = OutBuffer::around(buffer.as_mut_slice());
            self.0.run(&mut input, &mut output)?;
            let written = output.pos();
            piece.extend_from_slice(&buffer[..written]);
        }
        loop {
            let mut output = OutBuffer::around(buffer.as_mut_slice());
            let remaining = self.0.flush(&mut output)?;
            let written = output.pos();
            piece.extend_from_slice(&buffer[..written]);
            if remaining == 0 {
                return Ok(piece);
            }
        }
    }
}

pub struct ZstdStreamPieceDecompressor(Decoder<'static>);

impl ZstdStreamPieceDecompressor {
    pub fn new() -> io::Result<Self> {
        Decoder::new().map(Self)
    }

    pub fn decompress(&mut self, piece: &[u8]) -> io::Result<Vec<u8>> {
        let mut data = Vec::new();
        let mut buffer = vec![0; DCtx::out_size()];
        let mut input = InBuffer::around(piece);
        loop {
            let mut output = OutBuffer::around(buffer.as_mut_slice());
            self.0.run(&mut input, &mut output)?;
            let written = output.pos();
            data.extend_from_slice(&buffer[..written]);
            if input.pos() == piece.len() && written < buffer.len() {
                return Ok(data);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_piece_decodes_as_soon_as_it_arrives() {
        let mut compressor = ZstdStreamCompressor::new().unwrap();
        let mut decompressor = ZstdStreamPieceDecompressor::new().unwrap();
        let big = vec![7u8; 3 * CCtx::out_size()];
        for data in [&b"first"[..], b"", &big, b"last"] {
            let piece = compressor.compress(data).unwrap();
            assert_eq!(decompressor.decompress(&piece).unwrap(), data);
        }
    }
}
