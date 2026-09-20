//! Split a configuration line in a systemd-sysusers file into words.

use std::iter::Peekable;
use std::str::Chars;

use anyhow::{Context, Result, bail};

/// The characters systemd treats as whitespace in configuration files.
pub(super) const WHITESPACE: [char; 4] = [' ', '\t', '\n', '\r'];

/// Split `line` into words. Fails on an unbalanced quote or a trailing
/// backslash, like systemd.
pub(super) fn split(line: &str) -> Result<Vec<String>> {
    let mut chars = line.chars().peekable();
    let mut words = Vec::new();
    while let Some(&c) = chars.peek() {
        if WHITESPACE.contains(&c) {
            chars.next();
        } else {
            words.push(word(&mut chars)?);
        }
    }
    Ok(words)
}

/// Take one word, up to the whitespace after it.
fn word(chars: &mut Peekable<Chars<'_>>) -> Result<String> {
    let mut word = String::new();
    let mut quote = None;
    while let Some(c) = chars.next() {
        match c {
            '\\' => word.push(chars.next().context("trailing backslash")?),
            c if Some(c) == quote => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(c),
            c if quote.is_none() && WHITESPACE.contains(&c) => break,
            c => word.push(c),
        }
    }
    if let Some(quote) = quote {
        bail!("unbalanced quote ({quote})");
    }
    Ok(word)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_whitespace() -> Result<()> {
        assert_eq!(split("u  httpd\t404 ")?, ["u", "httpd", "404"]);
        assert!(split("   ")?.is_empty());
        assert_eq!(
            split("a\u{a0}b")?,
            ["a\u{a0}b"],
            "only ASCII whitespace separates words"
        );
        Ok(())
    }

    #[test]
    fn removes_quotes() -> Result<()> {
        assert_eq!(
            split(r#"u httpd 404 "HTTP User" 'single quoted'"#)?,
            ["u", "httpd", "404", "HTTP User", "single quoted"]
        );
        assert_eq!(
            split(r#"a"b c"d 'e"f' "g'h""#)?,
            ["ab cd", "e\"f", "g'h"],
            "quoted sections join the surrounding text"
        );
        assert_eq!(split(r#""" -"#)?, ["", "-"], "an empty quoted word is kept");
        Ok(())
    }

    #[test]
    fn backslash_makes_next_character_literal() -> Result<()> {
        assert_eq!(
            split(r#"a\ b \"c\" "d\"e" \\"#)?,
            ["a b", "\"c\"", "d\"e", "\\"]
        );
        Ok(())
    }

    #[test]
    fn rejects_unbalanced_quotes_and_trailing_backslash() {
        assert!(split(r#"u httpd "HTTP User"#).is_err());
        assert!(split("u httpd 'x").is_err());
        assert!(split(r"u httpd \").is_err());
    }
}
