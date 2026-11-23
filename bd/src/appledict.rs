//! Parser for Apple's `.dictionary` Body.data files.
//! Original format notes: https://gist.github.com/josephg/5e134adf70760ee7e49d
use anyhow::{bail, Context, Result};
use flate2::read::ZlibDecoder;
use std::io::Read;

fn int_at(d: &[u8], pos: usize) -> Result<i32> {
    let b = d.get(pos..pos + 4).context("Body.data truncated")?;
    Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Every definition (an `<d:entry ...>` XML string) in the file.
pub fn definitions(path: &str) -> Result<Vec<String>> {
    let data = std::fs::read(path).with_context(|| format!("reading {path}"))?;
    let limit = 0x40usize.saturating_add(int_at(&data, 0x40)?.max(0) as usize);
    // Two header layouts: body at 0x60 or at 0x44.
    let mut pos = if (int_at(&data, 0x44)?, int_at(&data, 0x48)?) == (0, -1) {
        0x60
    } else {
        0x44
    };
    let limit = limit.min(data.len());
    let mut out = Vec::new();
    while pos < limit {
        let compressed_size = int_at(&data, pos)? as i64 - 8;
        if compressed_size < 0 {
            bail!("corrupt section at offset {pos}");
        }
        let compressed_size = compressed_size as usize;
        let decompressed_size = int_at(&data, pos + 8)?.max(0) as usize;
        let start = pos.checked_add(12).context("corrupt section")?;
        let comp = data
            .get(start..start.checked_add(compressed_size).unwrap_or(usize::MAX))
            .context("section runs past end of file")?;
        let mut dec = Vec::with_capacity(decompressed_size.min(1 << 26));
        ZlibDecoder::new(comp).read_to_end(&mut dec)?;
        dec.truncate(decompressed_size);
        pos = start + compressed_size;

        if dec.starts_with(b"<d:entry") {
            out.push(String::from_utf8_lossy(&dec).into_owned());
        } else {
            let mut p = 0;
            while p + 4 <= dec.len() {
                let n = int_at(&dec, p)?.max(0) as usize;
                let end = (p + 4).saturating_add(n).min(dec.len());
                out.push(String::from_utf8_lossy(&dec[p + 4..end]).into_owned());
                p = end;
            }
        }
    }
    Ok(out)
}
