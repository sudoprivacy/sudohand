//! Minimal read-only SQLite reader — enough to walk one table of a Chromium
//! `Cookies` database (with its WAL applied) **without linking the C
//! library**, so the crate keeps cross-compiling with no C toolchain and no
//! `unsafe`. Supports table b-trees (interior + leaf), the overflow-page
//! chain for long records, every record serial type, and WAL frames up to
//! the last committed transaction. No SQL, no indexes: iterate a table's
//! rows and filter in Rust.
//!
//! References: the SQLite file format spec (§1.3 page format, §2 record
//! format, the WAL format).

use std::collections::HashMap;
use std::path::Path;

use crate::{Error, Result};

/// A decoded column value.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    /// `NULL`.
    Null,
    /// Any integer serial type.
    Int(i64),
    /// IEEE float.
    Real(f64),
    /// TEXT.
    Text(String),
    /// BLOB.
    Blob(Vec<u8>),
}

impl SqlValue {
    /// As text (BLOBs decoded lossily; numbers formatted).
    #[must_use]
    pub fn as_text(&self) -> String {
        match self {
            SqlValue::Null => String::new(),
            SqlValue::Int(i) => i.to_string(),
            SqlValue::Real(f) => f.to_string(),
            SqlValue::Text(s) => s.clone(),
            SqlValue::Blob(b) => String::from_utf8_lossy(b).into_owned(),
        }
    }
    /// As bytes (TEXT as UTF-8, BLOB verbatim, else empty).
    #[must_use]
    pub fn as_bytes(&self) -> Vec<u8> {
        match self {
            SqlValue::Text(s) => s.as_bytes().to_vec(),
            SqlValue::Blob(b) => b.clone(),
            _ => Vec::new(),
        }
    }
    /// As integer (0 for non-numbers).
    #[must_use]
    pub fn as_i64(&self) -> i64 {
        match self {
            SqlValue::Int(i) => *i,
            SqlValue::Real(f) => *f as i64,
            _ => 0,
        }
    }
}

fn be_u16(b: &[u8], o: usize) -> Result<u16> {
    b.get(o..o + 2)
        .map(|s| u16::from_be_bytes([s[0], s[1]]))
        .ok_or_else(|| Error::Invalid("sqlite: truncated (u16)".to_string()))
}

fn be_u32(b: &[u8], o: usize) -> Result<u32> {
    b.get(o..o + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| Error::Invalid("sqlite: truncated (u32)".to_string()))
}

/// SQLite varint: up to 9 bytes, big-endian 7-bit groups; the 9th byte
/// carries a full 8 bits. Returns `(value, bytes consumed)`.
fn varint(b: &[u8]) -> Result<(u64, usize)> {
    let mut v: u64 = 0;
    for i in 0..9 {
        let byte = *b
            .get(i)
            .ok_or_else(|| Error::Invalid("sqlite: truncated varint".to_string()))?;
        if i == 8 {
            v = (v << 8) | u64::from(byte);
            return Ok((v, 9));
        }
        v = (v << 7) | u64::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Ok((v, i + 1));
        }
    }
    Ok((v, 9))
}

/// Content byte length for a record serial type (§2.1).
fn serial_len(serial: u64) -> usize {
    match serial {
        0 | 8 | 9 => 0,
        1 => 1,
        2 => 2,
        3 => 3,
        4 => 4,
        5 => 6,
        6 | 7 => 8,
        n if n >= 12 && n % 2 == 0 => ((n - 12) / 2) as usize,
        n if n >= 13 => ((n - 13) / 2) as usize,
        _ => 0,
    }
}

fn decode_serial(serial: u64, data: &[u8]) -> SqlValue {
    match serial {
        0 => SqlValue::Null,
        1 => SqlValue::Int(i64::from(data[0] as i8)),
        2 => SqlValue::Int(i64::from(i16::from_be_bytes([data[0], data[1]]))),
        3 => {
            let mut b = [0u8; 4];
            b[1..].copy_from_slice(&data[..3]);
            let v = i32::from_be_bytes(b) << 8 >> 8; // sign-extend 24-bit
            SqlValue::Int(i64::from(v))
        }
        4 => SqlValue::Int(i64::from(i32::from_be_bytes([
            data[0], data[1], data[2], data[3],
        ]))),
        5 => {
            let mut b = [0u8; 8];
            b[2..].copy_from_slice(&data[..6]);
            let v = i64::from_be_bytes(b) << 16 >> 16; // sign-extend 48-bit
            SqlValue::Int(v)
        }
        6 => {
            let mut b = [0u8; 8];
            b.copy_from_slice(&data[..8]);
            SqlValue::Int(i64::from_be_bytes(b))
        }
        7 => {
            let mut b = [0u8; 8];
            b.copy_from_slice(&data[..8]);
            SqlValue::Real(f64::from_be_bytes(b))
        }
        8 => SqlValue::Int(0),
        9 => SqlValue::Int(1),
        n if n >= 12 && n % 2 == 0 => SqlValue::Blob(data.to_vec()),
        _ => SqlValue::Text(String::from_utf8_lossy(data).into_owned()),
    }
}

/// An open database: the main file with any committed WAL pages overlaid.
pub struct Database {
    data: Vec<u8>,
    page_size: usize,
    reserved: usize,
    wal_pages: HashMap<u32, Vec<u8>>,
}

impl Database {
    /// Open `path`, applying `<path>-wal` if present (up to the last commit).
    pub fn open(path: &Path) -> Result<Self> {
        let data = std::fs::read(path)?;
        if data.len() < 100 || &data[..16] != b"SQLite format 3\0" {
            return Err(Error::Invalid("not a SQLite database".to_string()));
        }
        let raw_ps = be_u16(&data, 16)?;
        let page_size = if raw_ps == 1 { 65_536 } else { raw_ps as usize };
        if page_size < 512 || !page_size.is_power_of_two() {
            return Err(Error::Invalid(format!("bad page size {page_size}")));
        }
        let reserved = data[20] as usize;
        let mut db = Self {
            data,
            page_size,
            reserved,
            wal_pages: HashMap::new(),
        };
        let wal = path.with_file_name(format!(
            "{}-wal",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
        if wal.exists() {
            let _ = db.apply_wal(&std::fs::read(&wal)?);
        }
        Ok(db)
    }

    /// Overlay the newest committed version of each page from the WAL.
    fn apply_wal(&mut self, wal: &[u8]) -> Result<()> {
        if wal.len() < 32 || !matches!(be_u32(wal, 0)?, 0x377f_0682 | 0x377f_0683) {
            return Ok(());
        }
        let ps = be_u32(wal, 8)? as usize;
        if ps != self.page_size {
            return Ok(());
        }
        let frame = 24 + ps;
        let mut off = 32;
        // Two-pass: only frames up to the last commit frame (db_size != 0) count.
        let mut latest: HashMap<u32, Vec<u8>> = HashMap::new();
        let mut committed: HashMap<u32, Vec<u8>> = HashMap::new();
        while off + frame <= wal.len() {
            let pgno = be_u32(wal, off)?;
            let db_size = be_u32(wal, off + 4)?;
            let page = wal[off + 24..off + 24 + ps].to_vec();
            latest.insert(pgno, page);
            if db_size != 0 {
                // Commit frame: snapshot everything seen so far.
                committed.clone_from(&latest);
            }
            off += frame;
        }
        self.wal_pages = committed;
        Ok(())
    }

    fn page(&self, pgno: u32) -> Result<&[u8]> {
        if let Some(p) = self.wal_pages.get(&pgno) {
            return Ok(p);
        }
        let start = (pgno as usize - 1) * self.page_size;
        self.data
            .get(start..start + self.page_size)
            .ok_or_else(|| Error::Invalid(format!("sqlite: page {pgno} out of range")))
    }

    /// Usable bytes per page (page size minus the reserved tail).
    fn usable(&self) -> usize {
        self.page_size - self.reserved
    }

    /// The rootpage of `table` from `sqlite_master` (page 1).
    pub fn table_root(&self, table: &str) -> Result<u32> {
        let mut root = None;
        self.walk_table(1, &mut |cols| {
            // sqlite_master: type, name, tbl_name, rootpage, sql
            if cols.len() >= 4 && cols[0].as_text() == "table" && cols[1].as_text() == table {
                root = Some(cols[3].as_i64() as u32);
            }
            true
        })?;
        root.ok_or_else(|| Error::Invalid(format!("table {table:?} not found")))
    }

    /// The `CREATE TABLE` SQL text for `table`, from `sqlite_master`.
    pub fn table_create_sql(&self, table: &str) -> Result<String> {
        let mut sql = None;
        self.walk_table(1, &mut |cols| {
            if cols.len() >= 5 && cols[0].as_text() == "table" && cols[1].as_text() == table {
                sql = Some(cols[4].as_text());
            }
            true
        })?;
        sql.ok_or_else(|| Error::Invalid(format!("no CREATE TABLE for {table:?}")))
    }

    /// Iterate every row of the table rooted at `root`, calling `f(cols)`;
    /// return `false` from `f` to stop early. Page 1's b-tree header sits at
    /// offset 100 (after the file header); all other pages at offset 0.
    pub fn walk_table<F: FnMut(&[SqlValue]) -> bool>(&self, root: u32, f: &mut F) -> Result<()> {
        self.walk_page(root, f)
    }

    fn walk_page<F: FnMut(&[SqlValue]) -> bool>(&self, pgno: u32, f: &mut F) -> Result<()> {
        let page = self.page(pgno)?;
        let hdr = if pgno == 1 { 100 } else { 0 };
        let page_type = page[hdr];
        let ncells = be_u16(page, hdr + 3)? as usize;
        let cell_ptr_base = hdr + if matches!(page_type, 5 | 2) { 12 } else { 8 };

        match page_type {
            13 => {
                // Table leaf.
                for i in 0..ncells {
                    let ptr = be_u16(page, cell_ptr_base + i * 2)? as usize;
                    let cols = self.read_leaf_cell(page, ptr)?;
                    if !f(&cols) {
                        return Ok(());
                    }
                }
            }
            5 => {
                // Table interior: each cell is (left child page, rowid).
                for i in 0..ncells {
                    let ptr = be_u16(page, cell_ptr_base + i * 2)? as usize;
                    let child = be_u32(page, ptr)?;
                    self.walk_page(child, f)?;
                }
                let right = be_u32(page, hdr + 8)?;
                self.walk_page(right, f)?;
            }
            _ => return Err(Error::Invalid(format!("unexpected page type {page_type}"))),
        }
        Ok(())
    }

    /// Read one table-leaf cell into its column values, following the
    /// overflow chain when the payload doesn't fit on the page.
    fn read_leaf_cell(&self, page: &[u8], ptr: usize) -> Result<Vec<SqlValue>> {
        let (payload_len, n1) = varint(&page[ptr..])?;
        let (_rowid, n2) = varint(&page[ptr + n1..])?;
        let header_start = ptr + n1 + n2;
        let payload_len = payload_len as usize;

        // Overflow threshold for table b-trees (spec §1.6).
        let u = self.usable();
        let max_local = u - 35;
        let min_local = (u - 12) * 32 / 255 - 23;
        let payload = if payload_len <= max_local {
            page[header_start..header_start + payload_len].to_vec()
        } else {
            let local = min_local + (payload_len - min_local) % (u - 4);
            let local = if local > max_local { min_local } else { local };
            let mut buf = page[header_start..header_start + local].to_vec();
            let mut next = be_u32(page, header_start + local)?;
            while next != 0 && buf.len() < payload_len {
                let op = self.page(next)?;
                next = be_u32(op, 0)?;
                let take = (payload_len - buf.len()).min(u - 4);
                buf.extend_from_slice(&op[4..4 + take]);
            }
            buf
        };
        decode_record(&payload)
    }
}

/// Decode a record payload (§2.1): header of serial types, then the values.
fn decode_record(payload: &[u8]) -> Result<Vec<SqlValue>> {
    let (hdr_len, n) = varint(payload)?;
    let hdr_len = hdr_len as usize;
    let mut serials = Vec::new();
    let mut off = n;
    while off < hdr_len {
        let (s, k) = varint(&payload[off..])?;
        serials.push(s);
        off += k;
    }
    let mut values = Vec::with_capacity(serials.len());
    let mut data_off = hdr_len;
    for s in serials {
        let len = serial_len(s);
        let slice = payload
            .get(data_off..data_off + len)
            .ok_or_else(|| Error::Invalid("sqlite: record body truncated".to_string()))?;
        values.push(decode_serial(s, slice));
        data_off += len;
    }
    Ok(values)
}

/// Extract column names, in order, from a `CREATE TABLE (...)` statement.
/// Good enough for Chromium's simple schemas: split the parenthesised body
/// on top-level commas and take each fragment's first identifier. Table
/// constraints (`PRIMARY KEY (...)`, `UNIQUE (...)`) start with a keyword and
/// are skipped.
#[must_use]
pub fn parse_create_columns(sql: &str) -> Vec<String> {
    let Some(open) = sql.find('(') else {
        return Vec::new();
    };
    let Some(close) = sql.rfind(')') else {
        return Vec::new();
    };
    let body = &sql[open + 1..close];
    let mut cols = Vec::new();
    let mut depth = 0i32;
    let mut frag = String::new();
    let flush = |frag: &str, cols: &mut Vec<String>| {
        let first = frag
            .trim()
            .trim_start_matches(['"', '`', '[', '\''])
            .split(|c: char| {
                c.is_whitespace() || c == '"' || c == '`' || c == '[' || c == ']' || c == '\''
            })
            .find(|t| !t.is_empty())
            .unwrap_or("");
        let upper = first.to_ascii_uppercase();
        if !first.is_empty()
            && !matches!(
                upper.as_str(),
                "PRIMARY" | "UNIQUE" | "CHECK" | "FOREIGN" | "CONSTRAINT" | "KEY"
            )
        {
            cols.push(first.to_string());
        }
    };
    for ch in body.chars() {
        match ch {
            '(' => {
                depth += 1;
                frag.push(ch);
            }
            ')' => {
                depth -= 1;
                frag.push(ch);
            }
            ',' if depth == 0 => {
                flush(&frag, &mut cols);
                frag.clear();
            }
            _ => frag.push(ch),
        }
    }
    flush(&frag, &mut cols);
    cols
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_and_serials() {
        assert_eq!(varint(&[0x00]).unwrap(), (0, 1));
        assert_eq!(varint(&[0x7f]).unwrap(), (127, 1));
        assert_eq!(varint(&[0x81, 0x00]).unwrap(), (128, 2));
        assert_eq!(serial_len(0), 0);
        assert_eq!(serial_len(6), 8);
        assert_eq!(serial_len(13), 0); // empty text
        assert_eq!(serial_len(15), 1); // 1-byte text
        assert_eq!(serial_len(14), 1); // 1-byte blob
    }

    #[test]
    fn parse_columns() {
        let sql = "CREATE TABLE cookies (creation_utc INTEGER NOT NULL, host_key TEXT, name TEXT, value TEXT, encrypted_value BLOB, path TEXT, is_secure INTEGER, is_httponly INTEGER, expires_utc INTEGER, UNIQUE (host_key, name, path))";
        let cols = parse_create_columns(sql);
        assert_eq!(cols[0], "creation_utc");
        assert_eq!(cols.iter().position(|c| c == "encrypted_value"), Some(4));
        assert_eq!(cols.iter().position(|c| c == "expires_utc"), Some(8));
        assert!(!cols.iter().any(|c| c == "UNIQUE"));
    }

    #[test]
    fn decode_a_record() {
        // header: len=3, serials [1 (int8), 21=0x15 (text, len 4)];
        // body: int8 0x07, then "abcd".
        let rec = [0x03, 0x01, 0x15, 0x07, b'a', b'b', b'c', b'd'];
        let cols = decode_record(&rec).unwrap();
        assert_eq!(cols[0], SqlValue::Int(7));
        assert_eq!(cols[1], SqlValue::Text("abcd".to_string()));
    }
}
