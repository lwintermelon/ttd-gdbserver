//! Packet-level helpers for the non-standard packets the stub answers itself
//! (`qRRCmd`, `qGetTLSAddr`): argument parsing and the hex codec RSP uses for
//! those payloads.
//!
//! Pure string/byte functions — unit-tested here instead of through a socket.

/// Parse qRRCmd arguments, handling both styles:
/// new: `:<cmd>:-1[:<hexarg>...]`, old: `:<hexcmd>[:<hexarg>...]`.
pub fn parse_qrrcmd_args(rest: &str) -> Vec<String> {
    let rest = rest.trim_start_matches(':');
    if rest.is_empty() {
        return Vec::new();
    }
    let parts: Vec<&str> = rest.split(':').collect();
    // New style: literal command followed by "-1".
    if parts.len() >= 2 && parts[1] == "-1" {
        let mut args = vec![parts[0].to_string()];
        for p in &parts[2..] {
            let decoded = decode_hex(p.as_bytes())
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_else(|| (*p).to_string());
            args.push(decoded);
        }
        return args;
    }
    // Old style: all tokens hex-encoded.
    parts
        .iter()
        .filter_map(|p| decode_hex(p.as_bytes()).map(|b| String::from_utf8_lossy(&b).into_owned()))
        .collect()
}

/// Parse a qGetTLSAddr thread-id fragment like `p1.100` or `100` into
/// the OS thread id (100). We do not currently honor multi-process pids
/// beyond the stub's single process.
pub fn parse_tid_from_qpacket(s: &str) -> Option<u64> {
    // Strip the optional `p<pid>.` prefix; what remains is the hex tid.
    if let Some(rest) = s.strip_prefix('p') {
        let (_pid, tail) = rest.split_once('.')?;
        return u64::from_str_radix(tail, 16).ok();
    }
    u64::from_str_radix(s, 16).ok()
}

/// Decode the lowercase/uppercase hex string RSP uses for binary-ish
/// payloads. `None` for odd length or non-hex digits.
pub fn decode_hex(s: &[u8]) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in s.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

/// Encode bytes as the lowercase hex string RSP expects (the inverse of
/// [`decode_hex`], used for `qRRCmd` replies).
pub fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qrrcmd_new_style_literal_command() {
        // Delve (>= 5.8) sends `qRRCmd:<cmd>:-1[:<hexarg>]`.
        assert_eq!(parse_qrrcmd_args(":when:-1"), vec!["when".to_string()]);
        assert_eq!(
            parse_qrrcmd_args(":info checkpoints:-1"),
            vec!["info checkpoints".to_string()]
        );
    }

    #[test]
    fn qrrcmd_new_style_hex_arguments() {
        // "delete checkpoint 1" — the id arrives hex-encoded ("31").
        assert_eq!(
            parse_qrrcmd_args(":delete checkpoint:-1:31"),
            vec!["delete checkpoint".to_string(), "1".to_string()]
        );
        // A trailing empty argument stays empty (checkpoint without a
        // "where" string).
        assert_eq!(
            parse_qrrcmd_args(":checkpoint:-1:"),
            vec!["checkpoint".to_string(), String::new()]
        );
    }

    #[test]
    fn qrrcmd_old_style_hex_command() {
        let when_hex = encode_hex(b"when");
        assert_eq!(parse_qrrcmd_args(&format!(":{when_hex}")), vec!["when"]);
    }

    #[test]
    fn qrrcmd_empty_argument_list() {
        assert!(parse_qrrcmd_args("").is_empty());
        assert!(parse_qrrcmd_args(":").is_empty());
    }

    /// A command name may itself contain a colon-separated tail that is not
    /// hex (e.g. a malformed client); it must degrade to the raw token rather
    /// than vanishing from the list.
    #[test]
    fn qrrcmd_old_style_non_hex_token_is_dropped_not_mangled() {
        assert!(parse_qrrcmd_args(":zz:yy").is_empty());
    }

    #[test]
    fn tid_from_qpacket() {
        assert_eq!(parse_tid_from_qpacket("p1.64"), Some(0x64));
        assert_eq!(parse_tid_from_qpacket("64"), Some(0x64));
        assert_eq!(parse_tid_from_qpacket("p1.dead"), Some(0xdead));
        // Missing pid separator / non-hex tid.
        assert_eq!(parse_tid_from_qpacket("p1"), None);
        assert_eq!(parse_tid_from_qpacket("nope"), None);
        assert_eq!(parse_tid_from_qpacket(""), None);
    }

    #[test]
    fn hex_roundtrip() {
        assert_eq!(encode_hex(b"\x00\x0f\xff"), "000fff");
        assert_eq!(decode_hex(b"000fff").unwrap(), b"\x00\x0f\xff");
        assert_eq!(decode_hex(b"DEAD").unwrap(), b"\xde\xad");
        assert_eq!(encode_hex(b""), "");
    }

    #[test]
    fn decode_hex_rejects_malformed_input() {
        assert_eq!(decode_hex(b"abc"), None, "odd length");
        assert_eq!(decode_hex(b"zz"), None, "non-hex digits");
    }
}
