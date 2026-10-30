//! LaTeX to MathML Core conversion.

use math_core::{LatexToMathML, MathCoreConfig, MathDisplay};
use pulldown_cmark_escape::{escape_html, escape_html_body_text};

pub struct MathRenderer {
    converter: LatexToMathML,
}

impl MathRenderer {
    pub fn new() -> Self {
        let config = MathCoreConfig {
            // Like MathJax in Obsidian, `\newcommand`s stay defined for the rest of the note.
            global_group: true,
            ..Default::default()
        };
        let converter = LatexToMathML::new(config, Vec::new()).expect("no custom macros");
        Self { converter }
    }

    /// Converts LaTeX to a `<math>` element. If the LaTeX is invalid, the source is shown in a
    /// `<code>` element instead and a warning is printed to stderr.
    pub fn render(&mut self, latex: &str, display: MathDisplay) -> String {
        match self.converter.convert_with_global_state(latex, display) {
            Ok(result) => result.mathml,
            Err(err) => {
                let message = err.to_string();
                eprintln!(
                    "blackrock: cannot render math `{}`: {message}",
                    latex.trim()
                );
                let mut html = String::from("<code class=\"math-error\" title=\"");
                escape_html(&mut html, &message).unwrap();
                html.push_str("\">");
                escape_html_body_text(&mut html, latex).unwrap();
                html.push_str("</code>");
                html
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_inline() {
        let html = MathRenderer::new().render("x^2", MathDisplay::Inline);
        assert_eq!(html, "<math><msup><mi>x</mi><mn>2</mn></msup></math>");
    }

    #[test]
    fn renders_block() {
        let html = MathRenderer::new().render(r"\frac{a}{b}", MathDisplay::Block);
        assert!(html.starts_with("<math display=\"block\">"), "{html}");
        assert!(html.contains("<mfrac>"), "{html}");
    }

    #[test]
    fn macros_persist() {
        let mut math = MathRenderer::new();
        math.render(r"\newcommand{\R}{\mathbb{R}}", MathDisplay::Inline);
        let html = math.render(r"x \in \R", MathDisplay::Inline);
        assert!(html.contains("ℝ"), "{html}");
    }

    #[test]
    fn invalid_math_falls_back_to_code() {
        let html = MathRenderer::new().render(r"\frac{a", MathDisplay::Inline);
        assert!(html.starts_with("<code class=\"math-error\""), "{html}");
        assert!(html.ends_with(r">\frac{a</code>"), "{html}");
    }
}
