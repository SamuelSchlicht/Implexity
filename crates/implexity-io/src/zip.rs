// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;

pub const STORED: u16 = 0;
pub const DEFLATED: u16 = 8;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ZipError(pub String);

fn err(msg: impl Into<String>) -> ZipError {
    ZipError(msg.into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipEntry {
    pub name: String,
    pub method: u16,
    pub crc32: u32,
    pub compressed_size: u64,
    pub size: u64,
    pub local_offset: u64,
    pub flags: u16,
    pub dos_time: u16,
    pub dos_date: u16,
    pub external_attr: u32,
    pub version_made_by: u16,
}

fn slice_at(b: &[u8], i: usize, len: usize) -> Option<&[u8]> {
    b.get(i..i.checked_add(len)?)
}

fn u16_at(b: &[u8], i: usize) -> Result<u16, ZipError> {
    slice_at(b, i, 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or_else(|| err("truncated archive"))
}

fn u32_at(b: &[u8], i: usize) -> Result<u32, ZipError> {
    slice_at(b, i, 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| err("truncated archive"))
}

fn u64_at(b: &[u8], i: usize) -> Result<u64, ZipError> {
    slice_at(b, i, 8)
        .map(|s| u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
        .ok_or_else(|| err("truncated archive"))
}

fn add(a: usize, b: usize) -> Result<usize, ZipError> {
    a.checked_add(b).ok_or_else(|| err("archive offset overflows"))
}

const MAX_DEFLATE_RATIO: u64 = 1032;
const MAX_PREALLOCATION: u64 = 64 * 1024 * 1024;

fn to_usize(v: u64) -> Result<usize, ZipError> {
    usize::try_from(v).map_err(|_| err("archive offset overflows"))
}

#[derive(Debug, Clone)]
pub struct ZipArchive<'a> {
    raw: &'a [u8],
    entries: Vec<ZipEntry>,
}

impl<'a> ZipArchive<'a> {

    pub fn new(raw: &'a [u8]) -> Result<Self, ZipError> {
        let tail_start = raw.len().saturating_sub(65_557);
        let eocd = raw[tail_start..]
            .windows(4)
            .rposition(|w| w == b"PK\x05\x06")
            .map(|p| p + tail_start)
            .ok_or_else(|| err("File is not a zip file"))?;
        let disk = u16_at(raw, eocd + 4)?;
        let cd_disk = u16_at(raw, eocd + 6)?;
        let mut total = u64::from(u16_at(raw, eocd + 10)?);
        let mut cd_size = u64::from(u32_at(raw, eocd + 12)?);
        let mut cd_offset = u64::from(u32_at(raw, eocd + 16)?);
        if disk != 0 || cd_disk != 0 {
            return Err(err("multi-disk archives are not supported"));
        }
        if (total == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_offset == 0xFFFF_FFFF) && eocd >= 20 {
            let loc = eocd - 20;
            if slice_at(raw, loc, 4) == Some(b"PK\x06\x07") {
                let rec = to_usize(u64_at(raw, loc + 8)?)?;
                if slice_at(raw, rec, 4) != Some(b"PK\x06\x06") {
                    return Err(err("corrupt ZIP64 end of central directory"));
                }
                total = u64_at(raw, add(rec, 32)?)?;
                cd_size = u64_at(raw, add(rec, 40)?)?;
                cd_offset = u64_at(raw, add(rec, 48)?)?;
            }
        }
        let mut entries = Vec::new();
        let mut p = to_usize(cd_offset)?;
        let end = p.checked_add(to_usize(cd_size)?).ok_or_else(|| err("archive offset overflows"))?;
        if end > raw.len() {
            return Err(err("truncated central directory"));
        }
        for _ in 0..total {
            if slice_at(raw, p, 4) != Some(b"PK\x01\x02") {
                return Err(err("Bad magic number for central directory"));
            }
            let version_made_by = u16_at(raw, p + 4)?;
            let flags = u16_at(raw, p + 8)?;
            let method = u16_at(raw, p + 10)?;
            let dos_time = u16_at(raw, p + 12)?;
            let dos_date = u16_at(raw, p + 14)?;
            let crc32 = u32_at(raw, p + 16)?;
            let mut compressed_size = u64::from(u32_at(raw, p + 20)?);
            let mut size = u64::from(u32_at(raw, p + 24)?);
            let name_len = usize::from(u16_at(raw, p + 28)?);
            let extra_len = usize::from(u16_at(raw, p + 30)?);
            let comment_len = usize::from(u16_at(raw, p + 32)?);
            let external_attr = u32_at(raw, p + 38)?;
            let mut local_offset = u64::from(u32_at(raw, p + 42)?);
            let name_bytes = slice_at(raw, add(p, 46)?, name_len).ok_or_else(|| err("truncated archive"))?;
            let name = if flags & 0x800 != 0 {
                String::from_utf8(name_bytes.to_vec()).map_err(|_| err("member name is not UTF-8"))?
            } else {

                name_bytes.iter().map(|&b| char::from(b)).collect()
            };
            let extra =
                slice_at(raw, add(p, 46 + name_len)?, extra_len).ok_or_else(|| err("truncated archive"))?;
            let mut q = 0;
            while q + 4 <= extra.len() {
                let id = u16::from_le_bytes([extra[q], extra[q + 1]]);
                let len = usize::from(u16::from_le_bytes([extra[q + 2], extra[q + 3]]));
                let body = extra.get(q + 4..q + 4 + len).ok_or_else(|| err("corrupt extra field"))?;
                if id == 1 {
                    let mut r = 0;
                    if size == 0xFFFF_FFFF {
                        size = u64_at(body, r)?;
                        r += 8;
                    }
                    if compressed_size == 0xFFFF_FFFF {
                        compressed_size = u64_at(body, r)?;
                        r += 8;
                    }
                    if local_offset == 0xFFFF_FFFF {
                        local_offset = u64_at(body, r)?;
                    }
                }
                q += 4 + len;
            }
            if flags & 1 != 0 {
                return Err(err(format!("member {name:?} is encrypted")));
            }
            entries.push(ZipEntry {
                name,
                method,
                crc32,
                compressed_size,
                size,
                local_offset,
                flags,
                dos_time,
                dos_date,
                external_attr,
                version_made_by,
            });
            p = add(p, 46 + name_len + extra_len + comment_len)?;
        }
        Ok(Self { raw, entries })
    }

    #[must_use]
    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.name.clone()).collect()
    }


    pub fn read(&self, name: &str) -> Result<Vec<u8>, ZipError> {
        let e = self
            .entries
            .iter()
            .find(|e| e.name == name)
            .ok_or_else(|| err(format!("There is no item named {name:?} in the archive")))?;
        self.read_entry(e)
    }


    pub fn read_entry(&self, e: &ZipEntry) -> Result<Vec<u8>, ZipError> {
        let lo = to_usize(e.local_offset)?;
        if slice_at(self.raw, lo, 4) != Some(b"PK\x03\x04") {
            return Err(err("Bad magic number for file header"));
        }
        let name_len = usize::from(u16_at(self.raw, add(lo, 26)?)?);
        let extra_len = usize::from(u16_at(self.raw, add(lo, 28)?)?);
        let start = add(lo, 30 + name_len + extra_len)?;
        let data = slice_at(self.raw, start, to_usize(e.compressed_size)?)
            .ok_or_else(|| err("truncated member data"))?;
        let out = match e.method {
            STORED => data.to_vec(),
            DEFLATED => {
                if e.size > (data.len() as u64).saturating_mul(MAX_DEFLATE_RATIO).saturating_add(64) {
                    return Err(err(format!("member {:?}: size mismatch", e.name)));
                }
                let mut out = Vec::with_capacity(to_usize(e.size.min(MAX_PREALLOCATION))?);
                DeflateDecoder::new(data)
                    .take(e.size.saturating_add(1))
                    .read_to_end(&mut out)
                    .map_err(|x| err(format!("member {:?}: invalid deflate stream: {x}", e.name)))?;
                out
            }
            m => return Err(err(format!("member {:?}: compression method {m} is not supported", e.name))),
        };
        if out.len() as u64 != e.size {
            return Err(err(format!("member {:?}: size mismatch", e.name)));
        }
        let mut crc = flate2::Crc::new();
        crc.update(&out);
        if crc.sum() != e.crc32 {
            return Err(err(format!("Bad CRC-32 for file {:?}", e.name)));
        }
        Ok(out)
    }
}

struct Written {
    name: Vec<u8>,
    method: u16,
    crc: u32,
    csize: u32,
    usize_: u32,
    offset: u32,
    flags: u16,
    dos_time: u16,
    dos_date: u16,
    external_attr: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberOptions {
    pub dos_time: u16,
    pub dos_date: u16,
    pub external_attr: u32,
}

impl MemberOptions {
    #[must_use]
    pub fn of(entry: &ZipEntry) -> Self {
        Self { dos_time: entry.dos_time, dos_date: entry.dos_date, external_attr: entry.external_attr }
    }

    #[must_use]
    pub fn now() -> Self {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
        let text = crate::digest::format_utc(secs);
        let num = |a: usize, b: usize| text.get(a..b).and_then(|t| t.parse::<u16>().ok()).unwrap_or(0);
        let (y, mo, d, h, mi, s) = (num(0, 4), num(5, 7), num(8, 10), num(11, 13), num(14, 16), num(17, 19));
        Self {
            dos_time: (h << 11) | (mi << 5) | (s / 2),
            dos_date: (y.saturating_sub(1980) << 9) | (mo << 5) | d,
            external_attr: 0o600 << 16,
        }
    }
}

pub struct ZipWriter {
    buf: Vec<u8>,
    entries: Vec<Written>,
    external_attr: u32,
}

impl Default for ZipWriter {
    fn default() -> Self {
        Self::new()
    }
}

pub const DOS_DATE_1980: u16 = (1 << 5) | 1;

impl ZipWriter {
    #[must_use]
    pub fn new() -> Self {
        Self { buf: Vec::new(), entries: Vec::new(), external_attr: 0o600 << 16 }
    }

    pub fn set_unix_mode(&mut self, mode: u32) {
        self.external_attr = mode << 16;
    }


    pub fn add(&mut self, name: &str, data: &[u8], method: u16, level: u32) -> Result<(), ZipError> {
        let options =
            MemberOptions { dos_time: 0, dos_date: DOS_DATE_1980, external_attr: self.external_attr };
        self.add_member(name, data, method, level, options)
    }


    pub fn add_member(
        &mut self,
        name: &str,
        data: &[u8],
        method: u16,
        level: u32,
        options: MemberOptions,
    ) -> Result<(), ZipError> {
        if self.entries.iter().any(|e| e.name == name.as_bytes()) {
            return Err(err(format!("duplicate member {name:?}")));
        }
        let mut crc = flate2::Crc::new();
        crc.update(data);
        let payload = match method {
            STORED => data.to_vec(),
            DEFLATED => {
                let mut enc = DeflateEncoder::new(Vec::new(), Compression::new(level.min(9)));
                enc.write_all(data).map_err(|e| err(e.to_string()))?;
                enc.finish().map_err(|e| err(e.to_string()))?
            }
            m => return Err(err(format!("compression method {m} is not supported"))),
        };
        let too_big = || err("members and archives of 4 GiB or more are not supported");
        let usize_ = u32::try_from(data.len()).map_err(|_| too_big())?;
        let csize = u32::try_from(payload.len()).map_err(|_| too_big())?;
        let offset = u32::try_from(self.buf.len()).map_err(|_| too_big())?;
        let flags: u16 = if name.is_ascii() { 0 } else { 0x800 };
        let name_len = u16::try_from(name.len()).map_err(|_| err("member name too long"))?;
        let b = &mut self.buf;
        b.extend_from_slice(b"PK\x03\x04");
        b.extend_from_slice(&20u16.to_le_bytes());
        b.extend_from_slice(&flags.to_le_bytes());
        b.extend_from_slice(&method.to_le_bytes());
        b.extend_from_slice(&options.dos_time.to_le_bytes());
        b.extend_from_slice(&options.dos_date.to_le_bytes());
        b.extend_from_slice(&crc.sum().to_le_bytes());
        b.extend_from_slice(&csize.to_le_bytes());
        b.extend_from_slice(&usize_.to_le_bytes());
        b.extend_from_slice(&name_len.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        b.extend_from_slice(name.as_bytes());
        b.extend_from_slice(&payload);
        self.entries.push(Written {
            name: name.as_bytes().to_vec(),
            method,
            crc: crc.sum(),
            csize,
            usize_,
            offset,
            flags,
            dos_time: options.dos_time,
            dos_date: options.dos_date,
            external_attr: options.external_attr,
        });
        Ok(())
    }


    pub fn finish(mut self) -> Result<Vec<u8>, ZipError> {
        let cd_start = u32::try_from(self.buf.len()).map_err(|_| err("archive too large"))?;
        for e in &self.entries {
            let b = &mut self.buf;
            b.extend_from_slice(b"PK\x01\x02");
            b.extend_from_slice(&((3u16 << 8) | 0x14).to_le_bytes());
            b.extend_from_slice(&20u16.to_le_bytes());
            b.extend_from_slice(&e.flags.to_le_bytes());
            b.extend_from_slice(&e.method.to_le_bytes());
            b.extend_from_slice(&e.dos_time.to_le_bytes());
            b.extend_from_slice(&e.dos_date.to_le_bytes());
            b.extend_from_slice(&e.crc.to_le_bytes());
            b.extend_from_slice(&e.csize.to_le_bytes());
            b.extend_from_slice(&e.usize_.to_le_bytes());
            b.extend_from_slice(&u16::try_from(e.name.len()).unwrap_or(u16::MAX).to_le_bytes());
            b.extend_from_slice(&0u16.to_le_bytes());
            b.extend_from_slice(&0u16.to_le_bytes());
            b.extend_from_slice(&0u16.to_le_bytes());
            b.extend_from_slice(&0u16.to_le_bytes());
            b.extend_from_slice(&e.external_attr.to_le_bytes());
            b.extend_from_slice(&e.offset.to_le_bytes());
            b.extend_from_slice(&e.name);
        }
        let cd_end = u32::try_from(self.buf.len()).map_err(|_| err("archive too large"))?;
        let count = u16::try_from(self.entries.len()).map_err(|_| err("too many members"))?;
        let b = &mut self.buf;
        b.extend_from_slice(b"PK\x05\x06");
        b.extend_from_slice(&0u16.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        b.extend_from_slice(&count.to_le_bytes());
        b.extend_from_slice(&count.to_le_bytes());
        b.extend_from_slice(&(cd_end - cd_start).to_le_bytes());
        b.extend_from_slice(&cd_start.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        Ok(self.buf)
    }
}


struct CrcSink<W> { output: W, crc: flate2::Crc }

impl<W: Write> Write for CrcSink<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let n = self.output.write(bytes)?;
        self.crc.update(&bytes[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> { self.output.flush() }
}

pub struct StreamZipWriter<W> { output: W, entries: Vec<Written> }

impl<W: Write + std::io::Seek> StreamZipWriter<W> {
    pub fn new(output: W) -> Self { Self { output, entries: Vec::new() } }

    pub fn add(&mut self, name: &str, size: u64, produce: impl FnOnce(&mut dyn Write) -> std::io::Result<()>) -> Result<(), ZipError> {
        let too_big = || err("NPZ ZIP32 representation requires each member and archive offset below 4 GiB");
        if self.entries.iter().any(|e| e.name == name.as_bytes()) { return Err(err("duplicate streamed ZIP member")); }
        let usize_ = u32::try_from(size).map_err(|_| too_big())?;
        let offset = u32::try_from(self.output.stream_position().map_err(|e| err(e.to_string()))?).map_err(|_| too_big())?;
        let name_len = u16::try_from(name.len()).map_err(|_| err("member name too long"))?;
        let flags: u16 = if name.is_ascii() {0} else {0x800};
        let mut header = Vec::with_capacity(30+name.len());
        header.extend_from_slice(b"PK\x03\x04");
        for word in [20u16,flags,DEFLATED,0,DOS_DATE_1980] {header.extend_from_slice(&word.to_le_bytes());}
        header.extend_from_slice(&[0;12]);header.extend_from_slice(&name_len.to_le_bytes());header.extend_from_slice(&0u16.to_le_bytes());header.extend_from_slice(name.as_bytes());
        self.output.write_all(&header).map_err(|e|err(e.to_string()))?;
        let start = self.output.stream_position().map_err(|e|err(e.to_string()))?;
        let (crc, actual) = {
            let mut sink = CrcSink { output: DeflateEncoder::new(&mut self.output, Compression::new(9)), crc: flate2::Crc::new() };
            produce(&mut sink).map_err(|e|err(e.to_string()))?;
            let crc = sink.crc.sum();
            let actual = sink.crc.amount();
            sink.output.finish().map_err(|e|err(e.to_string()))?;
            (crc, actual)
        };
        if actual != usize_ { return Err(err("streamed ZIP member length differs from declaration")); }
        let end = self.output.stream_position().map_err(|e|err(e.to_string()))?;
        let csize = u32::try_from(end-start).map_err(|_|too_big())?;
        self.output.seek(std::io::SeekFrom::Start(u64::from(offset)+14)).map_err(|e|err(e.to_string()))?;
        for word in [crc,csize,usize_] {self.output.write_all(&word.to_le_bytes()).map_err(|e|err(e.to_string()))?;}
        self.output.seek(std::io::SeekFrom::Start(end)).map_err(|e|err(e.to_string()))?;
        self.entries.push(Written { name:name.as_bytes().to_vec(), method:DEFLATED, crc,csize,usize_,offset,flags,dos_time:0,dos_date:DOS_DATE_1980,external_attr:0o600<<16 });
        Ok(())
    }

    pub fn finish(mut self) -> Result<W, ZipError> {
        let cd_start = u32::try_from(self.output.stream_position().map_err(|e|err(e.to_string()))?).map_err(|_|err("NPZ ZIP32 archive offset exceeds 4 GiB"))?;
        for e in &self.entries {
            let mut b=Vec::with_capacity(46+e.name.len());b.extend_from_slice(b"PK\x01\x02");
            for word in [(3u16<<8)|20,20,e.flags,e.method,e.dos_time,e.dos_date] {b.extend_from_slice(&word.to_le_bytes());}
            for word in [e.crc,e.csize,e.usize_] {b.extend_from_slice(&word.to_le_bytes());}
            for word in [e.name.len() as u16,0,0,0,0] {b.extend_from_slice(&word.to_le_bytes());}
            b.extend_from_slice(&e.external_attr.to_le_bytes());b.extend_from_slice(&e.offset.to_le_bytes());b.extend_from_slice(&e.name);self.output.write_all(&b).map_err(|e|err(e.to_string()))?;
        }
        let cd_end=u32::try_from(self.output.stream_position().map_err(|e|err(e.to_string()))?).map_err(|_|err("NPZ ZIP32 directory exceeds 4 GiB"))?;
        let count=u16::try_from(self.entries.len()).map_err(|_|err("too many ZIP members"))?;
        let mut b=Vec::with_capacity(22);b.extend_from_slice(b"PK\x05\x06");for word in [0,0,count,count] {b.extend_from_slice(&word.to_le_bytes());}b.extend_from_slice(&(cd_end-cd_start).to_le_bytes());b.extend_from_slice(&cd_start.to_le_bytes());b.extend_from_slice(&0u16.to_le_bytes());self.output.write_all(&b).and_then(|()|self.output.flush()).map_err(|e|err(e.to_string()))?;
        Ok(self.output)
    }
}

#[derive(Debug)]
pub struct FileZipIndex { entries: Vec<ZipEntry>, directory_offset: u64 }

impl FileZipIndex {
    pub fn new(file: &std::fs::File, max_fields: usize, max_directory_bytes: u64) -> Result<Self, ZipError> {
        use std::io::{Seek,SeekFrom};
        let mut reader=file;
        let size=reader.seek(SeekFrom::End(0)).map_err(|e|err(e.to_string()))?;
        let tail_len=size.min(65557) as usize;
        reader.seek(SeekFrom::End(-(tail_len as i64))).map_err(|e|err(e.to_string()))?;
        let mut tail=vec![0;tail_len];reader.read_exact(&mut tail).map_err(|e|err(e.to_string()))?;
        let marker=tail.windows(4).rposition(|w|w==b"PK\x05\x06").ok_or_else(||err("ZIP directory missing"))?;
        let end=&tail[marker..];
        if end.len()<22{return Err(err("truncated ZIP directory"));}
        let count=u16_at(end,10)?;let directory_bytes=u32_at(end,12)?;let directory_offset=u32_at(end,16)?;
        let eocd_offset=size-tail_len as u64+marker as u64;
        if u16_at(end,4)?!=0||u16_at(end,6)?!=0||u16_at(end,8)?!=count||count==0||count==u16::MAX||count as usize>max_fields
            ||directory_bytes==u32::MAX||directory_offset==u32::MAX||directory_bytes as u64>max_directory_bytes
            ||u64::from(directory_offset)+u64::from(directory_bytes)!=eocd_offset||end.len()!=22+u16_at(end,20)? as usize {
            return Err(err("ZIP directory exceeds its configured closed bounds"));
        }
        reader.seek(SeekFrom::Start(0)).map_err(|e|err(e.to_string()))?;let mut magic=[0;4];reader.read_exact(&mut magic).map_err(|e|err(e.to_string()))?;
        if &magic!=b"PK\x03\x04"{return Err(err("unsafe prepended ZIP payload"));}
        reader.seek(SeekFrom::Start(u64::from(directory_offset))).map_err(|e|err(e.to_string()))?;
        let mut index=vec![0;directory_bytes as usize];reader.read_exact(&mut index).map_err(|e|err(e.to_string()))?;
        let mut trailer=end.to_vec();trailer[16..20].copy_from_slice(&0u32.to_le_bytes());index.extend_from_slice(&trailer);
        let entries=ZipArchive::new(&index)?.entries().to_vec();
        if entries.len()!=count as usize{return Err(err("ZIP directory count differs"));}
        Ok(Self {entries,directory_offset:u64::from(directory_offset)})
    }

    #[must_use]
    pub fn entries(&self)->&[ZipEntry]{&self.entries}

    pub fn visit(&self,file:&std::fs::File,entry:&ZipEntry,mut consume:impl FnMut(&[u8])->Result<(),ZipError>)->Result<(),ZipError>{
        use std::io::{Seek,SeekFrom};
        let mut reader=file;reader.seek(SeekFrom::Start(entry.local_offset)).map_err(|e|err(e.to_string()))?;
        let mut header=[0;30];reader.read_exact(&mut header).map_err(|e|err(e.to_string()))?;
        if &header[..4]!=b"PK\x03\x04"||u16_at(&header,6)?!=entry.flags||u16_at(&header,8)?!=entry.method||u32_at(&header,14)?!=entry.crc32
            ||u64::from(u32_at(&header,18)?)!=entry.compressed_size||u64::from(u32_at(&header,22)?)!=entry.size{return Err(err("ZIP local and central metadata differ"));}
        let name_len=u16_at(&header,26)? as usize;let extra_len=u16_at(&header,28)? as u64;
        let mut name=vec![0;name_len];reader.read_exact(&mut name).map_err(|e|err(e.to_string()))?;
        if name!=entry.name.as_bytes(){return Err(err("ZIP local member name differs"));}
        let start=entry.local_offset.checked_add(30+name_len as u64).and_then(|n|n.checked_add(extra_len)).ok_or_else(||err("ZIP offset overflow"))?;
        if start.checked_add(entry.compressed_size).is_none_or(|n|n>self.directory_offset){return Err(err("ZIP member exceeds payload extent"));}
        reader.seek(SeekFrom::Start(start)).map_err(|e|err(e.to_string()))?;
        let bounded=reader.take(entry.compressed_size);
        let mut input:Box<dyn Read+'_>=match entry.method {STORED=>Box::new(bounded),DEFLATED=>Box::new(DeflateDecoder::new(bounded)),_=>return Err(err("unsupported streamed ZIP method"))};
        let mut crc=flate2::Crc::new();let mut total=0u64;let mut chunk=[0;65536];
        loop {let n=input.read(&mut chunk).map_err(|e|err(e.to_string()))?;if n==0{break;}total=total.checked_add(n as u64).ok_or_else(||err("ZIP member length overflow"))?;if total>entry.size{return Err(err("ZIP member exceeds declared length"));}crc.update(&chunk[..n]);consume(&chunk[..n])?;}
        if total!=entry.size||crc.sum()!=entry.crc32{return Err(err("ZIP member length or CRC mismatch"));}
        Ok(())
    }
}
