//! HTML escaping, matching `pulldown-cmark`'s HTML output.

use std::fmt::Write;

/// Escapes text for HTML element content.
pub fn text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// Escapes text for a quoted HTML attribute value.
pub fn attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Escapes a URL for an `href` or `src` attribute value. Bytes that aren't safe in a URL are
/// percent-encoded, except `&` and `'`, which are escaped as HTML.
pub fn href(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &byte in s.as_bytes() {
        match byte {
            b'&' => out.push_str("&amp;"),
            b'\'' => out.push_str("&#x27;"),
            b'!'
            | b'#'..=b'%'
            | b'('..=b';'
            | b'='
            | b'?'..=b'Z'
            | b'^'
            | b'_'
            | b'a'..=b'z'
            | b'~' => out.push(char::from(byte)),
            // Writing to a `String` can't fail.
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_text() {
        assert_eq!(
            text("a < b && \"c\" > 'd'"),
            "a &lt; b &amp;&amp; \"c\" &gt; 'd'"
        );
    }

    #[test]
    fn escapes_attributes() {
        assert_eq!(
            attr("a < b && \"c\" > 'd'"),
            "a &lt; b &amp;&amp; &quot;c&quot; &gt; &#39;d&#39;"
        );
    }

    #[test]
    fn escapes_urls() {
        assert_eq!(
            href("https://example.com/a b?x=1&y='2'#^é[|]"),
            "https://example.com/a%20b?x=1&amp;y=&#x27;2&#x27;#^%C3%A9%5B%7C%5D"
        );
        assert_eq!(href("\"<>\\`{}\u{7f}\n"), "%22%3C%3E%5C%60%7B%7D%7F%0A");
        assert_eq!(href("!$%()*+,-./:;=?@_~"), "!$%()*+,-./:;=?@_~");
    }
}
