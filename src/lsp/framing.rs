//! LSP stdio framing: `Content-Length` header blocks around JSON bodies.
//!
//! Promoted byte-for-byte in behavior from the spike transport. mcplease has
//! no LSP framer (its `stdio` is newline-delimited MCP, its `headers` is
//! HTTP), so this pair is hand-written. Both directions bound allocation: the
//! header block is read one byte at a time so the caps apply *before* any
//! body is sized from a hostile `Content-Length`.

use anyhow::{Context, Result, anyhow, bail};
use std::io::{BufRead, Write};

/// Upper bound on a single frame body, so a corrupt or hostile
/// `Content-Length` cannot make the reader allocate unbounded memory. 64 MiB
/// is far above any real LSP payload (references lists included).
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Upper bound on the header block, and on the number of header lines in it.
/// Both are tiny in practice; the caps exist so a wedged server cannot make
/// the client allocate without limit before the body cap applies.
pub const MAX_HEADER_BYTES: usize = 8 * 1024;
pub const MAX_HEADERS: usize = 64;

/// Read one LSP frame: an ASCII header block ending in a blank line, holding
/// `Content-Length`, followed by exactly that many bytes of JSON. `Ok(None)`
/// is a clean EOF at a frame boundary.
pub fn read_frame<R: BufRead>(reader: &mut R) -> Result<Option<Vec<u8>>> {
    let headers = match read_headers(reader)? {
        Some(headers) => headers,
        None => return Ok(None),
    };

    let mut content_length: Option<usize> = None;
    let mut header_count = 0usize;
    for line in headers.split(['\r', '\n']) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        header_count += 1;
        if header_count > MAX_HEADERS {
            bail!("header block has more than {MAX_HEADERS} headers");
        }
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            let value = rest.trim();
            content_length = Some(
                value
                    .parse()
                    .with_context(|| format!("bad Content-Length value `{value}`"))?,
            );
        }
        // Other headers (Content-Type) are legal and ignored.
    }

    let length = content_length.ok_or_else(|| anyhow!("header block had no Content-Length"))?;
    if length > MAX_FRAME_BYTES {
        bail!("Content-Length {length} exceeds the {MAX_FRAME_BYTES}-byte cap");
    }
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .with_context(|| format!("short body: expected {length} bytes"))?;
    Ok(Some(body))
}

/// Read the ASCII header block up to the blank line, one byte at a time so the
/// cap is enforced *before* allocating. `Ok(None)` is a clean EOF at a frame
/// boundary.
fn read_headers<R: BufRead>(reader: &mut R) -> Result<Option<String>> {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read_exact(&mut byte) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                if bytes.is_empty() {
                    return Ok(None);
                }
                bail!("EOF in the middle of a header block");
            }
            Err(error) => return Err(error.into()),
        }
        bytes.push(byte[0]);
        if bytes.len() > MAX_HEADER_BYTES {
            bail!("header block exceeds the {MAX_HEADER_BYTES}-byte cap");
        }
        if bytes.ends_with(b"\r\n\r\n") || bytes.ends_with(b"\n\n") {
            break;
        }
    }
    String::from_utf8(bytes)
        .map(Some)
        .context("header block was not valid UTF-8")
}

/// Write one framed message.
pub fn write_frame<W: Write>(writer: &mut W, body: &[u8]) -> Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn payload() -> &'static [u8] {
        br#"{"jsonrpc":"2.0","id":1,"result":null}"#
    }

    #[test]
    fn frame_round_trips() {
        let mut encoded = Vec::new();
        write_frame(&mut encoded, payload()).unwrap();
        let decoded = read_frame(&mut Cursor::new(encoded)).unwrap().unwrap();
        assert_eq!(decoded, payload());
    }

    #[test]
    fn frame_tolerates_extra_headers_and_crlf() {
        let body = payload();
        let mut encoded = format!(
            "Content-Length: {}\r\nContent-Type: application/json\r\n\r\n",
            body.len()
        )
        .into_bytes();
        encoded.extend_from_slice(body);
        let decoded = read_frame(&mut Cursor::new(encoded)).unwrap().unwrap();
        assert_eq!(decoded, body);
    }

    #[test]
    fn missing_content_length_is_an_error() {
        let encoded = b"Content-Type: application/json\r\n\r\n{}".to_vec();
        assert!(read_frame(&mut Cursor::new(encoded)).is_err());
    }

    #[test]
    fn oversize_length_is_rejected_without_allocating() {
        let encoded = format!("Content-Length: {}\r\n\r\n", MAX_FRAME_BYTES + 1).into_bytes();
        let error = read_frame(&mut Cursor::new(encoded)).unwrap_err();
        assert!(error.to_string().contains("exceeds"));
    }

    #[test]
    fn clean_eof_at_boundary_is_none() {
        assert!(read_frame(&mut Cursor::new(Vec::new())).unwrap().is_none());
    }

    #[test]
    fn eof_inside_a_body_is_an_error() {
        let encoded = b"Content-Length: 10\r\n\r\nshort".to_vec();
        assert!(read_frame(&mut Cursor::new(encoded)).is_err());
    }

    #[test]
    fn header_block_oversize_is_rejected() {
        let encoded = vec![b'x'; MAX_HEADER_BYTES + 1];
        let error = read_frame(&mut Cursor::new(encoded)).unwrap_err();
        assert!(error.to_string().contains("header block exceeds"));
    }

    #[test]
    fn too_many_headers_is_rejected() {
        let mut encoded = Vec::new();
        for index in 0..(MAX_HEADERS + 2) {
            encoded.extend_from_slice(format!("X-{index}: 1\r\n").as_bytes());
        }
        encoded.extend_from_slice(b"\r\n");
        encoded.extend_from_slice(b"{}");
        let error = read_frame(&mut Cursor::new(encoded)).unwrap_err();
        assert!(error.to_string().contains("more than"));
    }
}
