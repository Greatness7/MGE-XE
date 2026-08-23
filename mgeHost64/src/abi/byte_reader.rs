//! Strict byte-slice reader for generated binary assets.

use thiserror::Error;

#[derive(Clone, Debug, Error, PartialEq)]
#[error("unexpected EOF at byte {offset}: needed {needed} more bytes but only {remaining} remain")]
pub(crate) struct ByteReadError {
    pub(crate) offset: usize,
    pub(crate) needed: usize,
    pub(crate) remaining: usize,
}

pub(crate) struct ByteReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ByteReader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    pub(crate) fn offset(&self) -> usize {
        self.offset
    }

    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    pub(crate) fn read_exact_bytes(&mut self, count: usize) -> Result<&'a [u8], ByteReadError> {
        let start = self.offset;
        let end = start.checked_add(count).ok_or(ByteReadError {
            offset: start,
            needed: count,
            remaining: self.remaining(),
        })?;
        let slice = self.bytes.get(start..end).ok_or(ByteReadError {
            offset: start,
            needed: count,
            remaining: self.remaining(),
        })?;
        self.offset = end;
        Ok(slice)
    }

    pub(crate) fn skip(&mut self, count: usize) -> Result<(), ByteReadError> {
        let start = self.offset;
        let end = start.checked_add(count).ok_or(ByteReadError {
            offset: start,
            needed: count,
            remaining: self.remaining(),
        })?;
        if self.bytes.get(start..end).is_none() {
            return Err(ByteReadError {
                offset: start,
                needed: count,
                remaining: self.remaining(),
            });
        }
        self.offset = end;
        Ok(())
    }
}
