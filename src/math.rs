//! LaTeX to MathML Core conversion.

use math_core::{LatexToMathML, MathCoreConfig, MathDisplay};

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

    /// Converts LaTeX to a `<math>` element, or returns a description of why it can't.
    pub fn render(&mut self, latex: &str, display: MathDisplay) -> Result<String, String> {
        self.converter
            .convert_with_global_state(latex, display)
            .map(|result| result.mathml)
            .map_err(|err| format!("cannot render math `{}`: {err}", latex.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_inline() {
        let html = MathRenderer::new()
            .render("x^2", MathDisplay::Inline)
            .unwrap();
        assert_eq!(html, "<math><msup><mi>x</mi><mn>2</mn></msup></math>");
    }

    #[test]
    fn renders_block() {
        let html = MathRenderer::new()
            .render(r"\frac{a}{b}", MathDisplay::Block)
            .unwrap();
        assert!(html.starts_with("<math display=\"block\">"), "{html}");
        assert!(html.contains("<mfrac>"), "{html}");
    }

    #[test]
    fn macros_persist() {
        let mut math = MathRenderer::new();
        math.render(r"\newcommand{\myreals}{\mathbb{R}}", MathDisplay::Inline)
            .unwrap();
        let html = math.render(r"x \in \myreals", MathDisplay::Inline).unwrap();
        assert!(html.contains("ℝ"), "{html}");
    }

    #[test]
    fn invalid_math_is_an_error() {
        let err = MathRenderer::new()
            .render(r"\notacommand", MathDisplay::Inline)
            .unwrap_err();
        assert!(
            err.starts_with(r"cannot render math `\notacommand`: "),
            "{err}"
        );
    }
}
