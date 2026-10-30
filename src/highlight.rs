//! A small Rust syntax highlighter for the generated-code panel, plus background
//! highlights (in the selected node's colour) laid over it.

use eframe::egui::{text::LayoutJob, Color32, FontId, TextFormat};
use std::ops::Range;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tok {
    Keyword,
    Control,
    Type,
    Function,
    Macro,
    Str,
    Number,
    Comment,
    Lifetime,
    Attribute,
    Plain,
}

impl Tok {
    pub fn color(self) -> Color32 {
        match self {
            Tok::Keyword => Color32::from_rgb(86, 156, 214),
            Tok::Control => Color32::from_rgb(197, 134, 192),
            Tok::Type => Color32::from_rgb(78, 201, 176),
            Tok::Function => Color32::from_rgb(220, 220, 170),
            Tok::Macro => Color32::from_rgb(220, 220, 170),
            Tok::Str => Color32::from_rgb(206, 145, 120),
            Tok::Number => Color32::from_rgb(181, 206, 168),
            Tok::Comment => Color32::from_rgb(106, 153, 85),
            Tok::Lifetime => Color32::from_rgb(86, 156, 214),
            Tok::Attribute => Color32::from_rgb(156, 220, 254),
            Tok::Plain => Color32::from_rgb(212, 212, 212),
        }
    }
}

const CONTROL: &[&str] = &["if", "else", "match", "for", "while", "loop", "return", "break", "continue", "in", "await", "yield"];
const KEYWORDS: &[&str] = &[
    "fn", "let", "mut", "impl", "struct", "enum", "trait", "pub", "use", "mod", "const", "static", "as", "ref", "move",
    "where", "self", "Self", "crate", "super", "type", "unsafe", "async", "dyn", "true", "false", "extern", "macro_rules",
];

/// Split Rust source into (byte range, kind). Every byte belongs to exactly one token.
pub fn tokenize(src: &str) -> Vec<(Range<usize>, Tok)> {
    let b = src.as_bytes();
    let n = b.len();
    let mut out: Vec<(Range<usize>, Tok)> = vec![];
    let mut i = 0;
    let push = |out: &mut Vec<(Range<usize>, Tok)>, r: Range<usize>, t: Tok| {
        // merge plain runs to keep the layout small
        if let Some(last) = out.last_mut() {
            if last.1 == t && last.0.end == r.start && t == Tok::Plain {
                last.0.end = r.end;
                return;
            }
        }
        out.push((r, t));
    };
    while i < n {
        let c = b[i];
        let start = i;
        // comments
        if c == b'/' && i + 1 < n && b[i + 1] == b'/' {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            push(&mut out, start..i, Tok::Comment);
            continue;
        }
        if c == b'/' && i + 1 < n && b[i + 1] == b'*' {
            let mut depth = 0;
            while i < n {
                if b[i] == b'/' && i + 1 < n && b[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'*' && i + 1 < n && b[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            push(&mut out, start..i, Tok::Comment);
            continue;
        }
        // raw strings r"..", r#".."#, br#".."#
        let raw_at = if c == b'r' { Some(i + 1) } else if c == b'b' && i + 1 < n && b[i + 1] == b'r' { Some(i + 2) } else { None };
        if let Some(mut j) = raw_at {
            let mut hashes = 0;
            while j < n && b[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < n && b[j] == b'"' {
                j += 1;
                loop {
                    if j >= n {
                        break;
                    }
                    if b[j] == b'"' && (0..hashes).all(|k| j + 1 + k < n && b[j + 1 + k] == b'#') {
                        j += 1 + hashes;
                        break;
                    }
                    j += 1;
                }
                i = j;
                push(&mut out, start..i, Tok::Str);
                continue;
            }
        }
        // strings "..", b".."
        if c == b'"' || (c == b'b' && i + 1 < n && b[i + 1] == b'"') {
            i += if c == b'b' { 2 } else { 1 };
            while i < n && b[i] != b'"' {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(n);
            push(&mut out, start..i, Tok::Str);
            continue;
        }
        // char literal or lifetime
        if c == b'\'' {
            // 'x' / '\n' / '\u{..}'  vs  'a lifetime
            let rest = &src[i + 1..];
            let mut chars = rest.char_indices();
            if let Some((_, ch)) = chars.next() {
                if ch == '\\' {
                    let close = rest.find('\'').map(|p| p + 1).unwrap_or(rest.len());
                    i += 1 + close;
                    push(&mut out, start..i, Tok::Str);
                    continue;
                }
                if let Some((p, '\'')) = chars.next() {
                    i += 1 + p + 1;
                    push(&mut out, start..i, Tok::Str);
                    continue;
                }
                if ch.is_alphabetic() || ch == '_' {
                    i += 1;
                    while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                        i += 1;
                    }
                    push(&mut out, start..i, Tok::Lifetime);
                    continue;
                }
            }
            i += 1;
            push(&mut out, start..i, Tok::Plain);
            continue;
        }
        // attributes #[..] / #![..]
        if c == b'#' && i + 1 < n && (b[i + 1] == b'[' || b[i + 1] == b'!') {
            let mut depth = 0;
            while i < n {
                match b[i] {
                    b'[' => depth += 1,
                    b']' => {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    b'\n' if depth == 0 => break,
                    _ => {}
                }
                i += 1;
            }
            push(&mut out, start..i, Tok::Attribute);
            continue;
        }
        // numbers
        if c.is_ascii_digit() {
            while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || (b[i] == b'.' && i + 1 < n && b[i + 1].is_ascii_digit())) {
                i += 1;
            }
            push(&mut out, start..i, Tok::Number);
            continue;
        }
        // identifiers / keywords
        if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 {
            while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 0x80) {
                i += 1;
            }
            let word = &src[start..i];
            let next = src[i..].trim_start_matches(' ').as_bytes().first().copied();
            let t = if CONTROL.contains(&word) {
                Tok::Control
            } else if KEYWORDS.contains(&word) {
                Tok::Keyword
            } else if next == Some(b'!') && !src[i..].starts_with("!=") {
                Tok::Macro
            } else if word.starts_with(|ch: char| ch.is_uppercase()) {
                Tok::Type
            } else if next == Some(b'(') || src[i..].starts_with("::<") {
                Tok::Function
            } else {
                Tok::Plain
            };
            // include the `!` of a macro
            if t == Tok::Macro && i < n && b[i] == b'!' {
                i += 1;
            }
            push(&mut out, start..i, t);
            continue;
        }
        // anything else, one UTF-8 character at a time
        let len = src[i..].chars().next().map(|ch| ch.len_utf8()).unwrap_or(1);
        i += len;
        push(&mut out, start..i, Tok::Plain);
    }
    out
}

/// Lay out highlighted code. `marks` are byte ranges to paint with a background colour
/// (the selected node's colour); later marks win where they overlap.
pub fn layout(src: &str, font: FontId, marks: &[(Range<usize>, Color32)]) -> LayoutJob {
    let mut job = LayoutJob::default();
    // every position where the background might change
    let mut cuts: Vec<usize> = vec![];
    for (r, _) in marks {
        cuts.push(r.start);
        cuts.push(r.end);
    }
    for (range, tok) in tokenize(src) {
        let mut pieces = vec![range.start];
        pieces.extend(cuts.iter().copied().filter(|c| *c > range.start && *c < range.end));
        pieces.push(range.end);
        pieces.sort_unstable();
        pieces.dedup();
        for w in pieces.windows(2) {
            let (a, b) = (w[0], w[1]);
            if !src.is_char_boundary(a) || !src.is_char_boundary(b) {
                continue;
            }
            let bg = marks.iter().rev().find(|(r, _)| r.start <= a && a < r.end).map(|(_, c)| *c);
            let format = TextFormat {
                font_id: font.clone(),
                color: tok.color(),
                background: bg.unwrap_or(Color32::TRANSPARENT),
                ..Default::default()
            };
            job.append(&src[a..b], 0.0, format);
        }
    }
    job
}

/// Byte range of lines `first..=last` (0-based) in `src`, without the final newline.
pub fn line_range(src: &str, first: usize, last: usize) -> Range<usize> {
    let mut start = src.len();
    let mut end = src.len();
    let mut offset = 0;
    for (i, line) in src.split_inclusive('\n').enumerate() {
        if i == first {
            start = offset;
        }
        if i == last {
            end = offset + line.trim_end_matches('\n').len();
            break;
        }
        offset += line.len();
    }
    start.min(end)..end
}

/// Find `needle` inside `hay[within]`, ignoring differences in whitespace.
/// Returns the byte range in `hay`.
pub fn find_loose(hay: &str, within: Range<usize>, needle: &str) -> Option<Range<usize>> {
    let needle: Vec<char> = needle.chars().filter(|c| !c.is_whitespace()).collect();
    if needle.is_empty() {
        return None;
    }
    // (byte offset, char) of the non-space characters in the search area
    let chars: Vec<(usize, char)> =
        hay[within.clone()].char_indices().filter(|(_, c)| !c.is_whitespace()).map(|(i, c)| (i + within.start, c)).collect();
    if chars.len() < needle.len() {
        return None;
    }
    for s in 0..=chars.len() - needle.len() {
        if chars[s..s + needle.len()].iter().map(|(_, c)| *c).eq(needle.iter().copied()) {
            let (a, _) = chars[s];
            let (b, last) = chars[s + needle.len() - 1];
            return Some(a..b + last.len_utf8());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(String, Tok)> {
        tokenize(src).into_iter().filter(|(_, t)| *t != Tok::Plain).map(|(r, t)| (src[r].to_string(), t)).collect()
    }

    #[test]
    fn tokens() {
        let k = kinds("fn main() { let s: String = \"a\\\"b\"; // hi\n if x != 1.5 { println!(\"{}\", 'c'); } }");
        assert!(k.contains(&("fn".into(), Tok::Keyword)));
        assert!(k.contains(&("main".into(), Tok::Function)));
        assert!(k.contains(&("String".into(), Tok::Type)));
        assert!(k.contains(&("\"a\\\"b\"".into(), Tok::Str)));
        assert!(k.contains(&("// hi".into(), Tok::Comment)));
        assert!(k.contains(&("if".into(), Tok::Control)));
        assert!(k.contains(&("1.5".into(), Tok::Number)));
        assert!(k.contains(&("println!".into(), Tok::Macro)));
        assert!(k.contains(&("'c'".into(), Tok::Str)));
        let k = kinds("fn f<'a>(x: &'a str) -> &'static str { r#\"raw \" str\"# }");
        assert!(k.contains(&("'a".into(), Tok::Lifetime)));
        assert!(k.contains(&("r#\"raw \" str\"#".into(), Tok::Str)));
        // every byte is covered exactly once
        let src = "let x = \"é\" + 'ü'; /* a /* b */ c */ #[derive(Debug)]";
        let toks = tokenize(src);
        assert_eq!(toks.first().unwrap().0.start, 0);
        assert_eq!(toks.last().unwrap().0.end, src.len());
        assert!(toks.windows(2).all(|w| w[0].0.end == w[1].0.start));
    }

    #[test]
    fn loose_find() {
        let hay = "let total: i32 = v\n    .iter()\n    .sum();";
        let r = find_loose(hay, 0..hay.len(), "v.iter().sum()").unwrap();
        assert_eq!(&hay[r], "v\n    .iter()\n    .sum()");
        assert_eq!(line_range("a\nbb\nccc\n", 1, 2), 2..8);
    }
}
