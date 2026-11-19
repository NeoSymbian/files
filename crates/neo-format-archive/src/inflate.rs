// SPDX-License-Identifier: Apache-2.0
//! Raw deflate as a [`Stream`] over another stream, with the compressed bytes it read past the end kept (a gzip's
//! trailer and next member follow them).

use crate::{Error, Result, Stream};
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use miniz_oxide::inflate::stream::{InflateState, inflate};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};

const IN: usize = 32 * 1024;

pub struct Inflate<S> {
    src: S,
    st: Box<InflateState>,
    buf: Vec<u8>,
    lo: usize,
    hi: usize,
    /// The deflate stream ended.
    done: bool,
    /// The source ended.
    eof: bool,
}

impl<S: Stream> Inflate<S> {
    pub fn new(src: S) -> Self {
        Inflate {
            src,
            st: InflateState::new_boxed(DataFormat::Raw),
            buf: vec![0; IN],
            lo: 0,
            hi: 0,
            done: false,
            eof: false,
        }
    }

    /// A new deflate stream from where this one ended (a gzip's next member).
    pub fn restart(&mut self) {
        self.st.reset(DataFormat::Raw);
        self.done = false;
    }

    /// Bytes after the deflate stream: what it read ahead first, then the source.
    pub fn read_raw(&mut self, out: &mut [u8]) -> Result<usize> {
        if self.lo < self.hi {
            let n = out.len().min(self.hi - self.lo);
            out[..n].copy_from_slice(&self.buf[self.lo..self.lo + n]);
            self.lo += n;
            return Ok(n);
        }
        self.src.read(out)
    }

    pub fn into_inner(self) -> S {
        self.src
    }
}

/// [`Inflate::read_raw`] as a stream (headers and trailers).
pub(crate) struct Raw<'a, S>(pub &'a mut Inflate<S>);

impl<S: Stream> Stream for Raw<'_, S> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.0.read_raw(buf)
    }
}

impl<S: Stream> Stream for Inflate<S> {
    fn read(&mut self, out: &mut [u8]) -> Result<usize> {
        if self.done || out.is_empty() {
            return Ok(0);
        }
        loop {
            if self.lo == self.hi && !self.eof {
                self.lo = 0;
                self.hi = self.src.read(&mut self.buf)?;
                self.eof = self.hi == 0;
            }
            let r = inflate(
                &mut self.st,
                &self.buf[self.lo..self.hi],
                out,
                MZFlush::None,
            );
            self.lo += r.bytes_consumed;
            match r.status {
                Ok(MZStatus::StreamEnd) => {
                    self.done = true;
                    return Ok(r.bytes_written);
                }
                Ok(_) if r.bytes_written > 0 => return Ok(r.bytes_written),
                // input left, and none of it taken: it would spin
                Ok(_) if r.bytes_consumed == 0 && self.lo < self.hi => return Err(Error::Corrupt),
                // it wants more input
                Ok(_) | Err(MZError::Buf) if !self.eof => {}
                // the source ended inside the stream
                _ => return Err(Error::Corrupt),
            }
        }
    }
}
