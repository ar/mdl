//! Display a PNG in terminals that support the Kitty graphics protocol.
use std::env;
use std::io::{self, IsTerminal, Write};

pub fn supported() -> bool {
    if !io::stdout().is_terminal() || env::var_os("TMUX").is_some() || env::var_os("STY").is_some()
    {
        return false;
    }
    env::var("TERM_PROGRAM").is_ok_and(|s| s == "ghostty" || s == "kitty")
        || env::var("TERM").is_ok_and(|s| s == "xterm-ghostty" || s == "xterm-kitty")
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = *chunk.get(1).unwrap_or(&0);
        let c = *chunk.get(2).unwrap_or(&0);
        out.push(ALPHABET[(a >> 2) as usize] as char);
        out.push(ALPHABET[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn transmit(out: &mut impl Write, png: &[u8], columns: u16, rows: u16) -> io::Result<()> {
    let encoded = base64(png);
    let chunks: Vec<_> = encoded.as_bytes().chunks(4096).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        let more = u8::from(i + 1 < chunks.len());
        if i == 0 {
            write!(
                out,
                "\x1b_Ga=T,f=100,t=d,q=2,C=1,c={columns},r={rows},m={more};"
            )?;
        } else {
            write!(out, "\x1b_Gm={more};")?;
        }
        out.write_all(chunk)?;
        out.write_all(b"\x1b\\")?;
    }
    Ok(())
}

pub fn display(png: &[u8]) -> io::Result<()> {
    let columns = env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(100)
        .saturating_sub(1)
        .clamp(1, 120);
    let height = env::var("LINES")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(30);
    let rows = (columns / 4)
        .max(1)
        .min(height.saturating_sub(6).clamp(1, 24));
    let mut out = io::stdout().lock();
    for _ in 0..rows {
        writeln!(out)?;
    }
    write!(out, "\x1b[{rows}A\r")?;
    transmit(&mut out, png, columns, rows)?;
    write!(out, "\x1b[{rows}B\r")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_payload_uses_bounded_kitty_chunks() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
        let mut out = Vec::new();
        transmit(&mut out, &vec![42; 6000], 80, 20).unwrap();
        let text = String::from_utf8(out).unwrap();
        let chunks: Vec<_> = text.split("\x1b\\").filter(|s| !s.is_empty()).collect();
        assert!(chunks.len() > 1);
        assert!(chunks[0].starts_with("\x1b_Ga=T,f=100,t=d,q=2,C=1,c=80,r=20,m=1;"));
        assert!(chunks.last().unwrap().starts_with("\x1b_Gm=0;"));
        assert!(chunks
            .iter()
            .all(|c| c.split_once(';').unwrap().1.len() <= 4096));
    }
}
