use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};

/// Escapes text for HTML element content.
pub fn text(s: &str) -> String {
    // `&` goes first, so the `&`s of the other escapes aren't escaped again.
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Escapes text for a quoted HTML attribute value.
pub fn attr(s: &str) -> String {
    text(s).replace('"', "&quot;").replace('\'', "&#39;")
}

/// Bytes that aren't safe in a URL. Non-ASCII bytes are always encoded.
const NOT_URL_SAFE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'<')
    .add(b'>')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

/// Escapes a URL for an `href` or `src` attribute value. Bytes that aren't safe in a URL are
/// percent-encoded, and `&` and `'` are escaped as HTML.
pub fn href(s: &str) -> String {
    attr(&utf8_percent_encode(s, NOT_URL_SAFE).to_string())
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
            "https://example.com/a%20b?x=1&amp;y=&#39;2&#39;#^%C3%A9%5B%7C%5D"
        );
        assert_eq!(href("\"<>\\`{}\u{7f}\n"), "%22%3C%3E%5C%60%7B%7D%7F%0A");
        assert_eq!(href("!$%()*+,-./:;=?@_~"), "!$%()*+,-./:;=?@_~");
    }
}
