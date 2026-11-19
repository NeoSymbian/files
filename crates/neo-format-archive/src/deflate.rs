// SPDX-License-Identifier: Apache-2.0
//! Raw deflate fed in pieces, straight into a [`Sink`] (the zip and gzip writers).

use crate::{Error, Result, Sink};
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use miniz_oxide::deflate::core::{
    CompressorOxide, TDEFLFlush, TDEFLStatus, compress, create_comp_flags_from_zip_params,
};

pub(crate) struct Deflater {
    c: Box<CompressorOxide>,
    buf: Vec<u8>,
}

impl Deflater {
    /// Level 1–9; 6 is zlib's default.
    pub fn new(level: u8) -> Deflater {
        let flags = create_comp_flags_from_zip_params(level as i32, -15, 0);
        Deflater {
            c: Box::new(CompressorOxide::new(flags)),
            buf: vec![0; 32 * 1024],
        }
    }

    fn run(&mut self, mut data: &[u8], flush: TDEFLFlush, out: &mut impl Sink) -> Result<()> {
        loop {
            let (status, used, made) = compress(&mut self.c, data, &mut self.buf, flush);
            out.write(&self.buf[..made])?;
            data = &data[used..];
            match status {
                TDEFLStatus::Okay if !data.is_empty() || made == self.buf.len() => {}
                TDEFLStatus::Okay if flush == TDEFLFlush::Finish => {}
                TDEFLStatus::Okay | TDEFLStatus::Done => return Ok(()),
                _ => return Err(Error::Corrupt),
            }
        }
    }

    pub fn write(&mut self, data: &[u8], out: &mut impl Sink) -> Result<()> {
        self.run(data, TDEFLFlush::None, out)
    }

    pub fn finish(&mut self, out: &mut impl Sink) -> Result<()> {
        self.run(&[], TDEFLFlush::Finish, out)
    }
}
