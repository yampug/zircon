use std::path::{Path, PathBuf};

use lsp_types::Uri;

/// Convert an LSP `Uri` to a local file path.
pub fn to_path(uri: &Uri) -> Option<PathBuf> {
    let s = uri.as_str();
    let path_str = s.strip_prefix("file://")?;
    // Handle percent-encoded characters (basic: %20 → space).
    let decoded = percent_decode(path_str);
    Some(PathBuf::from(decoded))
}

/// Convert a local file path to an LSP `Uri`.
pub fn from_path(path: &Path) -> Option<Uri> {
    let s = format!("file://{}", path.display());
    s.parse().ok()
}

/// Minimal percent-decoding for file URIs.
fn percent_decode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next();
            let lo = chars.next();
            if let (Some(h), Some(l)) = (hi, lo) {
                if let (Some(hv), Some(lv)) = (hex_val(h), hex_val(l)) {
                    result.push((hv << 4 | lv) as char);
                    continue;
                }
            }
            result.push('%');
        } else {
            result.push(b as char);
        }
    }
    result
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip() {
        let path = Path::new("/tmp/src/app.cr");
        let uri = from_path(path).unwrap();
        let back = to_path(&uri).unwrap();
        assert_eq!(back, path);
    }

    #[test]
    fn test_percent_decode() {
        let uri: Uri = "file:///tmp/my%20project/app.cr".parse().unwrap();
        let path = to_path(&uri).unwrap();
        assert_eq!(path, PathBuf::from("/tmp/my project/app.cr"));
    }
}
