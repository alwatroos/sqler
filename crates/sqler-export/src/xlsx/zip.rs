// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A streaming zip writer: deflated entries with data descriptors, no `ZIP64`.
//!
//! An entry is written front to back without seeking: a local header with the sizes and CRC
//! left zero (flag bit 3), the deflated data, then a data descriptor. The central directory
//! and the end record come last. Nothing of an entry is kept in memory beyond the encoder.

use std::io::{self, Write};

use flate2::write::DeflateEncoder;
use flate2::{Compression, Crc};

use crate::ExportError;

const LOCAL_HEADER: u32 = 0x0403_4b50;
const DESCRIPTOR: u32 = 0x0807_4b50;
const CENTRAL_HEADER: u32 = 0x0201_4b50;
const END_RECORD: u32 = 0x0605_4b50;
/// Version 2.0: deflate and data descriptors.
const VERSION: u16 = 20;
/// Bit 3: sizes and CRC follow the data. Bit 11: the names are UTF-8.
const FLAGS: u16 = 0x0808;
const METHOD_DEFLATE: u16 = 8;
/// 1980-01-01, the earliest DOS date, so the output does not depend on the clock.
const DOS_DATE: u16 = 0x0021;
const DOS_TIME: u16 = 0;
/// `0xFFFFFFFF` in a size or an offset means "look in the ZIP64 extra field", so it is never
/// written as a real value.
const MAX_BYTES: u64 = u32::MAX as u64 - 1;

/// A size or offset as the 32-bit field, if it is below the `ZIP64` marker.
fn fit(v: u64) -> Result<u32, ExportError> {
    if v > MAX_BYTES {
        return Err(limit_error());
    }
    u32::try_from(v).map_err(|_| limit_error())
}

/// Counts the bytes that pass through.
struct Counter<W> {
    inner: W,
    written: u64,
}

impl<W: Write> Write for Counter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// What the central directory needs to know about a finished entry.
struct Record {
    name: String,
    crc: u32,
    compressed: u32,
    size: u32,
    offset: u32,
}

struct Open<W: Write> {
    encoder: DeflateEncoder<Counter<W>>,
    crc: Crc,
    size: u64,
    name: String,
    offset: u32,
    data_start: u64,
}

/// Writes a zip archive to `W` one entry at a time.
pub(super) struct ZipWriter<W: Write> {
    out: Option<Counter<W>>,
    open: Option<Open<W>>,
    records: Vec<Record>,
    limit: u64,
}

fn limit_error() -> ExportError {
    ExportError::Limit(
        "the XLSX file would pass 4 GiB, which this writer cannot store (no ZIP64); \
         export to CSV or Parquet instead"
            .to_owned(),
    )
}

fn put16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

impl<W: Write> ZipWriter<W> {
    pub(super) fn new(sink: W) -> Self {
        Self::with_limit(sink, MAX_BYTES)
    }

    /// A writer that fails with a limit error past `limit` bytes (at most `u32::MAX - 1`).
    pub(super) fn with_limit(sink: W, limit: u64) -> Self {
        Self {
            out: Some(Counter {
                inner: sink,
                written: 0,
            }),
            open: None,
            records: Vec::new(),
            limit: limit.min(MAX_BYTES),
        }
    }

    /// Starts an entry. The previous one must be finished.
    pub(super) fn start_entry(&mut self, name: &str) -> Result<(), ExportError> {
        let mut out = self
            .out
            .take()
            .ok_or(ExportError::State("a zip entry is still open"))?;
        let offset = fit(out.written)?;
        if out.written > self.limit {
            return Err(limit_error());
        }
        let name_len = u16::try_from(name.len())
            .map_err(|_| ExportError::InvalidOptions("zip entry name too long".to_owned()))?;
        let mut header = Vec::with_capacity(30 + name.len());
        put32(&mut header, LOCAL_HEADER);
        put16(&mut header, VERSION);
        put16(&mut header, FLAGS);
        put16(&mut header, METHOD_DEFLATE);
        put16(&mut header, DOS_TIME);
        put16(&mut header, DOS_DATE);
        put32(&mut header, 0);
        put32(&mut header, 0);
        put32(&mut header, 0);
        put16(&mut header, name_len);
        put16(&mut header, 0);
        header.extend_from_slice(name.as_bytes());
        out.write_all(&header)?;
        let data_start = out.written;
        self.open = Some(Open {
            encoder: DeflateEncoder::new(out, Compression::default()),
            crc: Crc::new(),
            size: 0,
            name: name.to_owned(),
            offset,
            data_start,
        });
        Ok(())
    }

    /// Appends data to the open entry.
    pub(super) fn write_data(&mut self, data: &[u8]) -> Result<(), ExportError> {
        let limit = self.limit;
        let open = self
            .open
            .as_mut()
            .ok_or(ExportError::State("no zip entry is open"))?;
        open.size += data.len() as u64;
        if open.size > limit {
            return Err(limit_error());
        }
        open.crc.update(data);
        open.encoder.write_all(data)?;
        if open.encoder.get_ref().written > limit {
            return Err(limit_error());
        }
        Ok(())
    }

    /// Finishes the open entry: flushes the encoder and writes the data descriptor.
    pub(super) fn end_entry(&mut self) -> Result<(), ExportError> {
        let open = self
            .open
            .take()
            .ok_or(ExportError::State("no zip entry is open"))?;
        let mut out = open.encoder.finish()?;
        let compressed = out.written - open.data_start;
        let (compressed, size) = (fit(compressed)?, fit(open.size)?);
        let crc = open.crc.sum();
        let mut descriptor = Vec::with_capacity(16);
        put32(&mut descriptor, DESCRIPTOR);
        put32(&mut descriptor, crc);
        put32(&mut descriptor, compressed);
        put32(&mut descriptor, size);
        out.write_all(&descriptor)?;
        if out.written > self.limit {
            return Err(limit_error());
        }
        self.records.push(Record {
            name: open.name,
            crc,
            compressed,
            size,
            offset: open.offset,
        });
        self.out = Some(out);
        Ok(())
    }

    /// Adds a complete entry.
    pub(super) fn add(&mut self, name: &str, data: &[u8]) -> Result<(), ExportError> {
        self.start_entry(name)?;
        self.write_data(data)?;
        self.end_entry()
    }

    /// Writes the central directory and the end record, flushes and returns the sink.
    pub(super) fn finish(mut self) -> Result<W, ExportError> {
        if self.open.is_some() {
            return Err(ExportError::State("a zip entry is still open"));
        }
        let mut out = self
            .out
            .take()
            .ok_or(ExportError::State("the zip was already finished"))?;
        let directory_offset = fit(out.written)?;
        let mut directory = Vec::new();
        for r in &self.records {
            put32(&mut directory, CENTRAL_HEADER);
            put16(&mut directory, VERSION);
            put16(&mut directory, VERSION);
            put16(&mut directory, FLAGS);
            put16(&mut directory, METHOD_DEFLATE);
            put16(&mut directory, DOS_TIME);
            put16(&mut directory, DOS_DATE);
            put32(&mut directory, r.crc);
            put32(&mut directory, r.compressed);
            put32(&mut directory, r.size);
            // The length was checked in `start_entry`.
            put16(&mut directory, u16::try_from(r.name.len()).unwrap_or(0));
            put16(&mut directory, 0);
            put16(&mut directory, 0);
            put16(&mut directory, 0);
            put16(&mut directory, 0);
            put32(&mut directory, 0);
            put32(&mut directory, r.offset);
            directory.extend_from_slice(r.name.as_bytes());
        }
        let count = u16::try_from(self.records.len()).map_err(|_| limit_error())?;
        let directory_size = fit(directory.len() as u64)?;
        put32(&mut directory, END_RECORD);
        put16(&mut directory, 0);
        put16(&mut directory, 0);
        put16(&mut directory, count);
        put16(&mut directory, count);
        put32(&mut directory, directory_size);
        put32(&mut directory, directory_offset);
        put16(&mut directory, 0);
        out.write_all(&directory)?;
        if out.written > self.limit {
            return Err(limit_error());
        }
        out.flush()?;
        Ok(out.inner)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use flate2::read::DeflateDecoder;

    use super::*;

    fn u16_at(b: &[u8], i: usize) -> u16 {
        u16::from_le_bytes([b[i], b[i + 1]])
    }

    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
    }

    /// Reads the archive through the central directory and checks every entry.
    fn read_back(zip: &[u8]) -> Vec<(String, Vec<u8>)> {
        let end = zip.len() - 22;
        assert_eq!(u32_at(zip, end), END_RECORD);
        let count = usize::from(u16_at(zip, end + 10));
        let mut at = u32_at(zip, end + 16) as usize;
        assert_eq!(at + u32_at(zip, end + 12) as usize, end);
        let mut entries = Vec::new();
        for _ in 0..count {
            assert_eq!(u32_at(zip, at), CENTRAL_HEADER);
            assert_eq!(u16_at(zip, at + 8), FLAGS);
            assert_eq!(u16_at(zip, at + 10), METHOD_DEFLATE);
            assert_eq!(u16_at(zip, at + 14), DOS_DATE);
            let (crc, csize, size) = (
                u32_at(zip, at + 16),
                u32_at(zip, at + 20) as usize,
                u32_at(zip, at + 24) as usize,
            );
            let name_len = usize::from(u16_at(zip, at + 28));
            let local = u32_at(zip, at + 42) as usize;
            let name = String::from_utf8(zip[at + 46..at + 46 + name_len].to_vec()).unwrap();
            // The local header leaves sizes and CRC zero; the descriptor carries them.
            assert_eq!(u32_at(zip, local), LOCAL_HEADER);
            assert_eq!(u16_at(zip, local + 6), FLAGS);
            assert_eq!(u32_at(zip, local + 14), 0);
            let local_name = usize::from(u16_at(zip, local + 26));
            assert_eq!(local_name, name_len);
            let data = local + 30 + name_len;
            let descriptor = data + csize;
            assert_eq!(u32_at(zip, descriptor), DESCRIPTOR);
            assert_eq!(u32_at(zip, descriptor + 4), crc);
            assert_eq!(u32_at(zip, descriptor + 8) as usize, csize);
            assert_eq!(u32_at(zip, descriptor + 12) as usize, size);
            let mut inflated = Vec::new();
            DeflateDecoder::new(&zip[data..descriptor])
                .read_to_end(&mut inflated)
                .unwrap();
            assert_eq!(inflated.len(), size);
            let mut check = Crc::new();
            check.update(&inflated);
            assert_eq!(check.sum(), crc);
            entries.push((name, inflated));
            at += 46 + name_len;
        }
        entries
    }

    #[test]
    fn the_zip64_marker_is_never_a_real_value() {
        assert!(fit(u64::from(u32::MAX) - 1).is_ok());
        assert!(matches!(
            fit(u64::from(u32::MAX)),
            Err(ExportError::Limit(_))
        ));
        assert_eq!(MAX_BYTES, u64::from(u32::MAX) - 1);
        let zip = ZipWriter::with_limit(Vec::new(), u64::MAX);
        assert_eq!(zip.limit, u64::from(u32::MAX) - 1);
    }

    #[test]
    fn entries_round_trip_with_matching_crc_and_sizes() {
        let big: Vec<u8> = (0..200_000_u32).flat_map(u32::to_le_bytes).collect();
        let mut zip = ZipWriter::new(Vec::new());
        zip.add("a.txt", b"hello").unwrap();
        zip.add("empty", b"").unwrap();
        zip.start_entry("dir/big.bin").unwrap();
        for chunk in big.chunks(7_001) {
            zip.write_data(chunk).unwrap();
        }
        zip.end_entry().unwrap();
        let bytes = zip.finish().unwrap();
        let entries = read_back(&bytes);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], ("a.txt".to_owned(), b"hello".to_vec()));
        assert_eq!(entries[1], ("empty".to_owned(), Vec::new()));
        assert_eq!(entries[2], ("dir/big.bin".to_owned(), big));
    }

    #[test]
    fn empty_archive_is_just_the_end_record() {
        let bytes = ZipWriter::new(Vec::new()).finish().unwrap();
        assert_eq!(bytes.len(), 22);
        assert_eq!(u32_at(&bytes, 0), END_RECORD);
    }

    #[test]
    fn calls_out_of_order_are_state_errors() {
        let mut zip = ZipWriter::new(Vec::new());
        assert!(matches!(zip.write_data(b"x"), Err(ExportError::State(_))));
        assert!(matches!(zip.end_entry(), Err(ExportError::State(_))));
        zip.start_entry("a").unwrap();
        assert!(matches!(zip.start_entry("b"), Err(ExportError::State(_))));
    }

    #[test]
    fn the_size_limit_is_an_error() {
        let mut zip = ZipWriter::with_limit(Vec::new(), 100);
        zip.add("a", b"small").unwrap();
        zip.start_entry("b").unwrap();
        // Incompressible-ish data: the uncompressed size alone passes the limit.
        let err = zip.write_data(&[7_u8; 200]).unwrap_err();
        assert!(matches!(&err, ExportError::Limit(m) if m.contains("CSV or Parquet")));
    }
}
