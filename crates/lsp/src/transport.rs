//! LSP message framing: `Content-Length: N\r\n\r\n<body>` over a byte stream.

use std::io::{self, BufRead};

/// Frame a JSON body as an LSP message (adds the `Content-Length` header).
pub fn encode(body: &str) -> Vec<u8> {
    format!("Content-Length: {}\r\n\r\n{}", body.len(), body).into_bytes()
}

/// Read one framed message body from `reader`. Returns `Ok(None)` on clean EOF.
pub fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut content_len: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None); // EOF
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // blank line terminates headers
        }
        if let Some(v) = trimmed
            .strip_prefix("Content-Length:")
            .or_else(|| trimmed.strip_prefix("content-length:"))
        {
            content_len = v.trim().parse::<usize>().ok();
        }
    }
    let len = content_len
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn encode_prepends_length_header() {
        let bytes = encode("{\"a\":1}");
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text, "Content-Length: 7\r\n\r\n{\"a\":1}");
    }

    #[test]
    fn round_trips_through_read_message() {
        let mut stream = Vec::new();
        stream.extend(encode("{\"jsonrpc\":\"2.0\"}"));
        stream.extend(encode("{\"id\":1}"));
        let mut cursor = Cursor::new(stream);
        assert_eq!(
            read_message(&mut cursor).unwrap().as_deref(),
            Some("{\"jsonrpc\":\"2.0\"}")
        );
        assert_eq!(
            read_message(&mut cursor).unwrap().as_deref(),
            Some("{\"id\":1}")
        );
        assert_eq!(read_message(&mut cursor).unwrap(), None); // EOF
    }

    #[test]
    fn tolerates_extra_headers() {
        let msg = "Content-Type: application/json\r\nContent-Length: 2\r\n\r\n{}";
        let mut cursor = Cursor::new(msg.as_bytes().to_vec());
        assert_eq!(read_message(&mut cursor).unwrap().as_deref(), Some("{}"));
    }
}
