//! Caller-controlled bytes, and the only path that turns them into displayable text.
//!
//! [`Untrusted`] deliberately implements neither `Display` nor `Deref<Target = str>`, so it cannot
//! be interpolated into a format string, concatenated with trusted prose, or handed to anything
//! expecting a `&str`. The single way to get it on screen is to escape it into an [`Escaped`] and
//! pass that to the crate's dialog builder.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use unicode_general_category::{get_general_category, GeneralCategory};

/// Cap on the rendered length of a single token. Far above any realistic argument.
pub const MAX_TOKEN_CHARS: usize = 4096;

/// Bytes that came from outside the trust boundary: argv, cwd, environment values.
#[derive(Clone, PartialEq, Eq)]
pub struct Untrusted(Vec<u8>);

impl Untrusted {
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Untrusted(bytes.into())
    }

    pub fn from_os(s: &OsStr) -> Self {
        Untrusted(s.as_bytes().to_vec())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn byte_len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for Untrusted {
    /// Escaped, so that a debug print can never emit raw control bytes.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Untrusted({:?})", Escaped::of(self).as_str())
    }
}

/// Text that is safe to put on screen: escaped caller data, compiled-in prose, or numbers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Escaped {
    text: String,
    escaped: bool,
    truncated: bool,
}

impl Escaped {
    /// Compiled-in prose. `&'static str` is the type-level evidence that it is not caller data.
    pub fn literal(s: &'static str) -> Self {
        Escaped { text: s.to_string(), escaped: false, truncated: false }
    }

    /// A number the gate computed itself.
    pub fn number(n: u64) -> Self {
        Escaped { text: n.to_string(), escaped: false, truncated: false }
    }

    pub fn concat(parts: impl IntoIterator<Item = Escaped>) -> Self {
        let mut out = Escaped { text: String::new(), escaped: false, truncated: false };
        for p in parts {
            out.text.push_str(&p.text);
            out.escaped |= p.escaped;
            out.truncated |= p.truncated;
        }
        out
    }

    /// Escape untrusted bytes for display.
    pub fn of(u: &Untrusted) -> Self {
        escape_bytes(&u.0, MAX_TOKEN_CHARS)
    }

    /// Escape untrusted bytes and shell-quote them, for rendering one argv token.
    pub fn shell_token(u: &Untrusted) -> Self {
        quoted(u, |b| unquoted_ok(b))
    }

    /// Clip to `max_chars`, flagging the result as truncated.
    ///
    /// The cut can land mid-escape or inside a quote. That is deliberate: an unterminated `'` or
    /// `$'` is a shell syntax error, which is the failure to prefer over a clipped token that
    /// still parses as some *other* command.
    pub fn clipped(mut self, max_chars: usize) -> Self {
        if self.text.chars().count() <= max_chars {
            return self;
        }
        self.text = self.text.chars().take(max_chars).collect();
        self.truncated = true;
        self
    }

    /// A path for display. Quoted like a token, except that a `~` — which the gate substitutes for
    /// the requester's home directory — is not on its own a reason to quote.
    pub fn path(u: &Untrusted) -> Self {
        quoted(u, |b| unquoted_ok(b) || *b == b'~')
    }

    /// `NAME=value`, both halves escaped.
    pub fn assignment(name: &Untrusted, value: &Untrusted) -> Self {
        Escaped::concat([Escaped::of(name), Escaped::literal("="), Escaped::of(value)])
    }

    /// True if any byte had to be neutralised. A literal backslash is doubled for
    /// unambiguity but does not count: the flag means "unsafe content was escaped".
    pub fn was_escaped(&self) -> bool {
        self.escaped
    }

    pub fn was_truncated(&self) -> bool {
        self.truncated
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }
}

/// Escape, then quote so the result reads back as exactly this one token in a shell. Three forms,
/// cheapest first:
///
/// - bare, when every byte is safe unquoted;
/// - `'…'`, when it is not. Exact for the ordinary cases — a space, a quote, `$`, a glob;
/// - `$'…'`, when the escaper had to write a backslash. Bash decodes `\xNN`/`\uNNNN`/`\\` inside
///   that form and would take them literally inside `'…'`, so a token with a newline in it would
///   otherwise be shown as a command that pastes back as a *different* string.
///
/// Empty is always quoted: an empty token has to look like one rather than like a missing value.
fn quoted(u: &Untrusted, ok: impl Fn(&u8) -> bool) -> Escaped {
    let mut e = escape_bytes(&u.0, MAX_TOKEN_CHARS);
    // Every backslash in the rendered text is one the escaper wrote, and every escape it writes
    // starts with one — so this is exactly "was anything neutralised or doubled".
    if e.text.contains('\\') {
        e.text = format!("$'{}'", e.text.replace('\'', "\\'"));
    } else if u.0.is_empty() || !u.0.iter().all(ok) {
        e.text = format!("'{}'", e.text.replace('\'', "'\\''"));
    }
    e
}

/// Bytes that need no shell quoting to be read back as one token.
fn unquoted_ok(b: &u8) -> bool {
    matches!(b,
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
        | b'_' | b'@' | b'%' | b'+' | b'=' | b':' | b',' | b'.' | b'/' | b'-')
}

/// How a single decoded character is rendered. `None` means "keep it verbatim".
fn escape_char(c: char) -> Option<String> {
    match c {
        // A literal backslash is doubled so that `\xNN` in the output is unambiguously an
        // escape we produced. Not flagged as escaping, see `Escaped::was_escaped`.
        '\\' => Some("\\\\".to_string()),
        // Printable ASCII, space included, is left alone.
        ' '..='~' => None,
        // C0 controls and DEL. `\xNN` is reserved for single bytes, so anything above ASCII
        // uses `\uNNNN` — otherwise U+0085 and the stray byte 0x85 would render identically.
        _ if (c as u32) < 0x20 || c as u32 == 0x7f => Some(format!("\\x{:02x}", c as u32)),
        _ => match get_general_category(c) {
            // C1 controls.
            GeneralCategory::Control => Some(codepoint(c)),
            // Bidi controls, zero width joiners, unassigned and private-use codepoints.
            GeneralCategory::Format
            | GeneralCategory::Surrogate
            | GeneralCategory::PrivateUse
            | GeneralCategory::Unassigned => Some(codepoint(c)),
            // U+2028/U+2029 are mandatory breaks to Pango; non-ASCII spaces fake alignment.
            GeneralCategory::LineSeparator
            | GeneralCategory::ParagraphSeparator
            | GeneralCategory::SpaceSeparator => Some(codepoint(c)),
            // Ordinary printable non-ASCII (café.txt) stays readable.
            _ => None,
        },
    }
}

/// A codepoint escape in bash's `$'...'` spelling: exactly four hex digits, or exactly eight above
/// the BMP. Fixed width is what makes it unambiguous — bash reads at most four digits after a `\u`,
/// so a literal hex digit following an escape is never swallowed into it.
fn codepoint(c: char) -> String {
    if (c as u32) <= 0xffff {
        format!("\\u{:04x}", c as u32)
    } else {
        format!("\\U{:08x}", c as u32)
    }
}

fn escape_bytes(bytes: &[u8], limit: usize) -> Escaped {
    let mut out = String::new();
    let mut escaped = false;
    let mut truncated = false;

    // Appends `s`, or reports that the limit was hit.
    let push = |out: &mut String, s: &str| -> bool {
        if out.chars().count() + s.chars().count() > limit {
            false
        } else {
            out.push_str(s);
            true
        }
    };

    let mut i = 0;
    'outer: while i < bytes.len() {
        let (valid_len, bad_len) = match std::str::from_utf8(&bytes[i..]) {
            Ok(_) => (bytes.len() - i, 0),
            Err(e) => (e.valid_up_to(), e.error_len().unwrap_or(bytes.len() - i - e.valid_up_to())),
        };

        if valid_len > 0 {
            let s = std::str::from_utf8(&bytes[i..i + valid_len]).expect("validated above");
            for c in s.chars() {
                match escape_char(c) {
                    None => {
                        if !push(&mut out, c.encode_utf8(&mut [0u8; 4])) {
                            truncated = true;
                            break 'outer;
                        }
                    }
                    Some(rep) => {
                        // Doubling a backslash is not "escaping" in the flagged sense.
                        if c != '\\' {
                            escaped = true;
                        }
                        if !push(&mut out, &rep) {
                            truncated = true;
                            break 'outer;
                        }
                    }
                }
            }
        }

        // Invalid UTF-8: overlong encodings, encoded surrogates, stray bytes. Per byte, never
        // lossily replaced — distinct requests must not render identically.
        for b in &bytes[i + valid_len..i + valid_len + bad_len] {
            escaped = true;
            if !push(&mut out, &format!("\\x{:02x}", b)) {
                truncated = true;
                break 'outer;
            }
        }

        i += valid_len + bad_len;
    }

    Escaped { text: out, escaped, truncated }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn esc(bytes: &[u8]) -> Escaped {
        Escaped::of(&Untrusted::from_bytes(bytes.to_vec()))
    }

    #[test]
    fn printable_ascii_survives() {
        let e = esc(b"/usr/bin/ls -l");
        assert_eq!(e.as_str(), "/usr/bin/ls -l");
        assert!(!e.was_escaped());
    }

    #[test]
    fn printable_non_ascii_survives() {
        let e = esc("café.txt".as_bytes());
        assert_eq!(e.as_str(), "café.txt");
        assert!(!e.was_escaped());
    }

    #[test]
    fn controls_escaped() {
        let e = esc(b"a\nb\tc\x1b[0m\x7f");
        assert_eq!(e.as_str(), "a\\x0ab\\x09c\\x1b[0m\\x7f");
        assert!(e.was_escaped());
    }

    #[test]
    fn c1_controls_escaped() {
        let e = esc("\u{85}".as_bytes());
        assert_eq!(e.as_str(), "\\u0085");
        assert!(e.was_escaped());
    }

    #[test]
    fn backslash_doubled_but_not_flagged() {
        let e = esc(b"a\\x41");
        assert_eq!(e.as_str(), "a\\\\x41");
        assert!(!e.was_escaped());
    }

    #[test]
    fn invalid_utf8_escaped_per_byte() {
        let e = esc(b"a\xffb\xc3");
        assert_eq!(e.as_str(), "a\\xffb\\xc3");
        assert!(e.was_escaped());
    }

    #[test]
    fn overlong_encoding_escaped_per_byte() {
        // Overlong encoding of '/'.
        let e = esc(&[0xc0, 0xaf]);
        assert_eq!(e.as_str(), "\\xc0\\xaf");
    }

    #[test]
    fn encoded_surrogate_escaped_per_byte() {
        // CESU-8 style encoding of U+D800.
        let e = esc(&[0xed, 0xa0, 0x80]);
        assert_eq!(e.as_str(), "\\xed\\xa0\\x80");
    }

    #[test]
    fn distinct_invalid_sequences_render_differently() {
        assert_ne!(esc(&[0xff]).as_str(), esc(&[0xfe]).as_str());
        // A stray 0x85 byte and the C1 control U+0085 must not collide.
        assert_ne!(esc(&[0x85]).as_str(), esc("\u{85}".as_bytes()).as_str());
    }

    #[test]
    fn separators_and_odd_spaces_escaped() {
        assert_eq!(esc("a\u{2028}b".as_bytes()).as_str(), "a\\u2028b");
        assert_eq!(esc("a\u{2029}b".as_bytes()).as_str(), "a\\u2029b");
        assert_eq!(esc("a\u{00a0}b".as_bytes()).as_str(), "a\\u00a0b");
        assert_eq!(esc("a\u{3000}b".as_bytes()).as_str(), "a\\u3000b");
    }

    #[test]
    fn format_and_unassigned_escaped() {
        assert_eq!(esc("a\u{200b}b".as_bytes()).as_str(), "a\\u200bb");
        assert_eq!(esc("a\u{202e}b".as_bytes()).as_str(), "a\\u202eb");
        assert_eq!(esc("a\u{200d}b".as_bytes()).as_str(), "a\\u200db");
        assert_eq!(esc("a\u{0378}b".as_bytes()).as_str(), "a\\u0378b");
    }

    #[test]
    fn ascii_space_is_kept() {
        assert_eq!(esc(b"a b").as_str(), "a b");
    }

    #[test]
    fn truncation_is_flagged() {
        let big = vec![b'x'; MAX_TOKEN_CHARS + 10];
        let e = esc(&big);
        assert!(e.was_truncated());
        assert_eq!(e.as_str().chars().count(), MAX_TOKEN_CHARS);
    }

    #[test]
    fn shell_quoting() {
        let q = |b: &[u8]| Escaped::shell_token(&Untrusted::from_bytes(b.to_vec())).text;
        assert_eq!(q(b"/usr/bin/ls"), "/usr/bin/ls");
        assert_eq!(q(b"a b"), "'a b'");
        assert_eq!(q(b""), "''");
        assert_eq!(q(b"it's"), "'it'\\''s'");
        assert_eq!(q(b"-l"), "-l");
        assert_eq!(q(b"$(x)"), "'$(x)'");
        // `~` is a shell metacharacter, so an argv token containing one is still quoted.
        assert_eq!(q(b"~/x"), "'~/x'");
    }

    /// A token the escaper had to write a backslash into gets bash's ANSI-C form, where those
    /// escapes mean what they say. In `'…'` they would paste back as literal backslash text, so
    /// the line on screen would be a command that runs something else.
    #[test]
    fn escaped_tokens_use_ansi_c_quoting() {
        let q = |b: &[u8]| Escaped::shell_token(&Untrusted::from_bytes(b.to_vec())).text;
        assert_eq!(q(b"a\nb"), "$'a\\x0ab'");
        assert_eq!(q(b"a\\b"), "$'a\\\\b'");
        assert_eq!(q(b"it's\nnow"), "$'it\\'s\\x0anow'");
        assert_eq!(q(&[0xff]), "$'\\xff'");
        assert_eq!(q("\u{2028}".as_bytes()), "$'\\u2028'");
        // Fixed-width escapes: bash takes two hex digits after \x and four after \u, so an
        // adjacent literal hex digit cannot be swallowed into the escape.
        assert_eq!(q(b"\nb"), "$'\\x0ab'");
        assert_eq!(q("\u{200b}b".as_bytes()), "$'\\u200bb'");
    }

    /// The property the gate's one-line command field promises: paste what is on screen into bash
    /// and you get back exactly the argv the gate was asked to run. Asserted against a real bash
    /// rather than against our own idea of its rules.
    #[test]
    fn rendered_tokens_paste_back_as_the_same_argv() {
        let argv: Vec<Vec<u8>> = vec![
            b"/usr/bin/rm".to_vec(),
            b"-rf".to_vec(),
            b"/tmp/a b".to_vec(),
            b"x'y".to_vec(),
            b"two\nlines".to_vec(),
            b"back\\slash".to_vec(),
            b"$(id)".to_vec(),
            b"~/x".to_vec(),
            "caf\u{e9}.txt".as_bytes().to_vec(),
            // Escaped: a C1 control, a zero-width space, a private-use codepoint above the BMP,
            // and bytes that are not UTF-8 at all.
            "\u{85}\u{200b}\u{f0000}".as_bytes().to_vec(),
            vec![0xff, 0xfe],
            // A hex digit right after an escape, where a variable-width escape would over-read.
            b"\ncafe".to_vec(),
            b"".to_vec(),
        ];
        let line = argv
            .iter()
            .map(|a| Escaped::shell_token(&Untrusted::from_bytes(a.clone())).text)
            .collect::<Vec<_>>()
            .join(" ");

        let Ok(out) = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!("printf '%s\\0' {line}"))
            .output()
        else {
            return; // No bash here; nothing to check it against.
        };
        assert!(out.status.success(), "bash rejected: {line}\n{:?}", out.stderr);

        let mut back: Vec<Vec<u8>> = out.stdout.split(|b| *b == 0).map(|s| s.to_vec()).collect();
        // printf writes a NUL after the last token, so the split leaves one empty tail.
        back.pop();
        assert_eq!(back, argv, "rendered as: {line}");
    }

    #[test]
    fn clipping_flags_and_shortens() {
        let e = Escaped::literal("abcdef").clipped(3);
        assert_eq!(e.as_str(), "abc");
        assert!(e.was_truncated());
        let whole = Escaped::literal("abc").clipped(3);
        assert_eq!(whole.as_str(), "abc");
        assert!(!whole.was_truncated());
    }

    #[test]
    fn path_quoting_tolerates_the_substituted_tilde() {
        let p = |b: &[u8]| Escaped::path(&Untrusted::from_bytes(b.to_vec())).text;
        assert_eq!(p(b"~/desktop-bits"), "~/desktop-bits");
        assert_eq!(p(b"/etc"), "/etc");
        assert_eq!(p(b"~/my dir"), "'~/my dir'");
        assert_eq!(p(b""), "''");
    }

    #[test]
    fn markup_is_literal_text_after_escaping() {
        // No escaping needed: the dialog renders with markup off, so these stay literal.
        let e = esc(b"<span foreground='#101014'>hidden</span>");
        assert_eq!(e.as_str(), "<span foreground='#101014'>hidden</span>");
    }

    #[test]
    fn concat_propagates_flags() {
        let e = Escaped::concat([Escaped::literal("uid "), Escaped::number(1006), esc(b"\x01")]);
        assert_eq!(e.as_str(), "uid 1006\\x01");
        assert!(e.was_escaped());
    }
}
