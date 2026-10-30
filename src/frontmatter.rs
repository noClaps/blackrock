//! YAML frontmatter extraction and conversion to JSON.

use serde_json::{Map, Value as Json};

/// Splits a document into its raw YAML frontmatter (if any) and the remaining markdown body.
///
/// Frontmatter must start on the very first line with `---` and end with a line containing only
/// `---` (or `...`).
pub fn split(source: &str) -> (Option<&str>, &str) {
    let text = source.strip_prefix('\u{feff}').unwrap_or(source);
    let Some((first, rest)) = text.split_once('\n') else {
        return (None, source);
    };
    if first.trim_end() != "---" {
        return (None, source);
    }
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if matches!(line.trim_end(), "---" | "...") {
            return (Some(&rest[..offset]), &rest[offset + line.len()..]);
        }
        offset += line.len();
    }
    (None, source)
}

/// Parses YAML frontmatter into a JSON value. Empty frontmatter becomes an empty object.
pub fn to_json(yaml: Option<&str>) -> Result<Json, serde_yaml_ng::Error> {
    Ok(match serde_yaml_ng::from_str(yaml.unwrap_or_default())? {
        Json::Null => Json::Object(Map::new()),
        json => json,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_frontmatter() {
        let (yaml, body) = split("---\ntitle: Hi\n---\n# Body\n");
        assert_eq!(yaml, Some("title: Hi\n"));
        assert_eq!(body, "# Body\n");
    }

    #[test]
    fn no_frontmatter() {
        let (yaml, body) = split("# Body\n---\n");
        assert_eq!(yaml, None);
        assert_eq!(body, "# Body\n---\n");
    }

    #[test]
    fn unterminated_frontmatter_is_body() {
        let (yaml, body) = split("---\ntitle: Hi\n");
        assert_eq!(yaml, None);
        assert_eq!(body, "---\ntitle: Hi\n");
    }

    #[test]
    fn converts_to_json() {
        let json = to_json(Some("title: Hi\ntags: [a, b]\n1: one\ndate: 2024-01-01\n")).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"title": "Hi", "tags": ["a", "b"], "1": "one", "date": "2024-01-01"})
        );
    }

    #[test]
    fn empty_frontmatter_is_object() {
        assert_eq!(to_json(Some("")).unwrap(), serde_json::json!({}));
        assert_eq!(to_json(None).unwrap(), serde_json::json!({}));
    }
}
