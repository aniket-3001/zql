//! The file header, and reading pages by number.
//!
//! Pages are read on demand rather than the whole file being slurped: a browser
//! history database is tens of megabytes, and a photo library index can be much
//! more than that. Nothing here holds more than one page at a time.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::error::{Result, SqlState, ZqlError};
use crate::sources::sqlite::record::{corrupt, TextEncoding};

const MAGIC: &[u8; 16] = b"SQLite format 3\0";
const HEADER_SIZE: usize = 100;

/// The bytes of the file header that zql actually reads.
#[derive(Debug, Clone, Copy)]
pub struct Header {
    pub page_size: u32,
    /// `page_size` minus the reserved tail, which is the space a page really
    /// has. Extensions such as encryption claim the reserved bytes.
    pub usable_size: u32,
    pub page_count: u32,
    pub encoding: TextEncoding,
}

impl Header {
    fn parse(bytes: &[u8], file_len: u64) -> Result<Header> {
        if bytes.len() < HEADER_SIZE {
            return Err(not_sqlite("file is shorter than a database header"));
        }
        if &bytes[..16] != MAGIC {
            return Err(not_sqlite("bad magic"));
        }

        // Offset 16, a u16, with one special case: the value 1 means 65536,
        // because 65536 does not fit in the field.
        let raw_page_size = u16::from_be_bytes([bytes[16], bytes[17]]);
        let page_size: u32 = match raw_page_size {
            1 => 65_536,
            size if size >= 512 && size.is_power_of_two() => u32::from(size),
            other => return Err(corrupt(format!("implausible page size {other}"))),
        };

        // Offset 20: bytes reserved at the end of every page.
        let reserved = u32::from(bytes[20]);
        let usable_size = page_size
            .checked_sub(reserved)
            .filter(|usable| *usable >= 480)
            .ok_or_else(|| corrupt("reserved space leaves an unusable page"))?;

        if reserved != 0 {
            // Encryption and some extensions claim these bytes and change what
            // the rest of the page means. Refusing is the honest answer.
            return Err(ZqlError::unsupported(
                "SQLite files with reserved page space",
            )
            .with_detail(
                "this usually means the database is encrypted or uses an extension",
            ));
        }

        let page_count = u32::from_be_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]);
        // The in-header page count is only trustworthy when the change counter
        // and version-valid-for match; falling back to the file size is what
        // SQLite itself does, and it is safer than trusting a stale field.
        let page_count = if page_count == 0 {
            u32::try_from(file_len / u64::from(page_size)).unwrap_or(0)
        } else {
            page_count
        };

        let encoding_value =
            u32::from_be_bytes([bytes[56], bytes[57], bytes[58], bytes[59]]);

        Ok(Header {
            page_size,
            usable_size,
            page_count,
            encoding: TextEncoding::from_header_value(encoding_value)?,
        })
    }
}

/// Reads pages out of one database file.
pub struct Pager {
    file: File,
    pub header: Header,
}

impl Pager {
    pub fn open(path: &Path) -> Result<Pager> {
        let mut file = File::open(path).map_err(|err| {
            ZqlError::new(
                SqlState::IoError,
                format!("cannot open {}: {err}", path.display()),
            )
        })?;

        let file_len = file.metadata().map(|meta| meta.len()).unwrap_or(0);

        let mut header_bytes = [0u8; HEADER_SIZE];
        file.read_exact(&mut header_bytes)
            .map_err(|_| not_sqlite("file is shorter than a database header"))?;

        let header = Header::parse(&header_bytes, file_len)?;

        Ok(Pager { file, header })
    }

    /// Reads one page by its **one-based** number.
    ///
    /// Page 1 is special: it carries the 100-byte file header in front of its
    /// b-tree header. Every other page starts its b-tree header at offset 0.
    /// Getting this wrong breaks only page 1 — which is `sqlite_master`, which
    /// is the first thing anything reads.
    pub fn read_page(&mut self, page_number: u32) -> Result<Page> {
        if page_number == 0 || page_number > self.header.page_count {
            return Err(corrupt(format!(
                "page {page_number} is outside the database"
            )));
        }

        let offset = u64::from(page_number - 1) * u64::from(self.header.page_size);
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|err| corrupt(format!("cannot seek to page {page_number}: {err}")))?;

        let mut bytes = vec![0u8; self.header.page_size as usize];
        self.file
            .read_exact(&mut bytes)
            .map_err(|err| corrupt(format!("cannot read page {page_number}: {err}")))?;

        Ok(Page {
            bytes,
            number: page_number,
            usable_size: self.header.usable_size as usize,
        })
    }
}

/// One page, and where its b-tree content begins.
pub struct Page {
    pub bytes: Vec<u8>,
    pub number: u32,
    pub usable_size: usize,
}

impl Page {
    /// The offset at which this page's b-tree header starts.
    pub fn header_offset(&self) -> usize {
        if self.number == 1 {
            HEADER_SIZE
        } else {
            0
        }
    }

    pub fn byte_at(&self, offset: usize) -> Result<u8> {
        self.bytes
            .get(offset)
            .copied()
            .ok_or_else(|| corrupt("read past the end of a page"))
    }

    pub fn u16_at(&self, offset: usize) -> Result<u16> {
        let bytes = self
            .bytes
            .get(offset..offset + 2)
            .ok_or_else(|| corrupt("read past the end of a page"))?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    pub fn u32_at(&self, offset: usize) -> Result<u32> {
        let bytes = self
            .bytes
            .get(offset..offset + 4)
            .ok_or_else(|| corrupt("read past the end of a page"))?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// The page's contents, truncated to the usable size — so a reserved tail,
    /// if one were ever allowed, could never be mistaken for payload.
    pub fn usable(&self) -> &[u8] {
        let end = self.usable_size.min(self.bytes.len());
        &self.bytes[..end]
    }
}

fn not_sqlite(reason: &str) -> ZqlError {
    ZqlError::new(
        SqlState::IoError,
        format!("not a SQLite database ({reason})"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_bytes(page_size: u16, reserved: u8) -> Vec<u8> {
        let mut bytes = vec![0u8; HEADER_SIZE];
        bytes[..16].copy_from_slice(MAGIC);
        bytes[16..18].copy_from_slice(&page_size.to_be_bytes());
        bytes[20] = reserved;
        bytes[28..32].copy_from_slice(&4u32.to_be_bytes());
        bytes[56..60].copy_from_slice(&1u32.to_be_bytes());
        bytes
    }

    #[test]
    fn a_non_sqlite_file_is_rejected_by_magic() {
        let bytes = vec![b'n'; HEADER_SIZE];
        let error = Header::parse(&bytes, 4096).unwrap_err();
        assert!(error.message.contains("not a SQLite database"));
    }

    #[test]
    fn a_short_file_is_rejected_rather_than_indexed_into() {
        assert!(Header::parse(&[0u8; 10], 10).is_err());
    }

    #[test]
    fn page_size_one_means_sixty_five_thousand_five_hundred_and_thirty_six() {
        let header = Header::parse(&header_bytes(1, 0), 65_536 * 4).unwrap();
        assert_eq!(header.page_size, 65_536);
        assert_eq!(header.usable_size, 65_536);
    }

    #[test]
    fn ordinary_page_sizes_parse() {
        for size in [512u16, 1024, 4096, 8192] {
            let header = Header::parse(&header_bytes(size, 0), 4096 * 4).unwrap();
            assert_eq!(header.page_size, u32::from(size));
        }
    }

    #[test]
    fn a_page_size_that_is_not_a_power_of_two_is_refused() {
        assert!(Header::parse(&header_bytes(1000, 0), 4096).is_err());
        assert!(Header::parse(&header_bytes(256, 0), 4096).is_err());
    }

    #[test]
    fn reserved_page_space_is_refused_by_name() {
        let error = Header::parse(&header_bytes(4096, 20), 4096 * 4).unwrap_err();
        assert_eq!(error.state, SqlState::FeatureNotSupported);
        assert!(error.detail.unwrap().contains("encrypted"));
    }

    #[test]
    fn page_one_reserves_room_for_the_file_header() {
        let page = Page {
            bytes: vec![0; 4096],
            number: 1,
            usable_size: 4096,
        };
        assert_eq!(page.header_offset(), HEADER_SIZE);

        let page = Page {
            bytes: vec![0; 4096],
            number: 2,
            usable_size: 4096,
        };
        assert_eq!(page.header_offset(), 0);
    }

    #[test]
    fn reads_past_the_end_of_a_page_are_errors() {
        let page = Page {
            bytes: vec![0; 8],
            number: 2,
            usable_size: 8,
        };
        assert!(page.u32_at(6).is_err());
        assert!(page.u16_at(7).is_err());
        assert!(page.byte_at(8).is_err());
        assert!(page.u32_at(4).is_ok());
    }


}
