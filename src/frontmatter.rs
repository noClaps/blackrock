//! YAML frontmatter extraction and conversion to JSON.

use serde_json::{Map, Number, Value as Json};
use serde_yaml_ng::Value as Yaml;

/// Splits a document into its raw YAML frontmatter (if any) and the remaining markdown body.
///
/// Frontmatter must start on the very first line with `---` and end with a line containing only
/// `---` (or `...`).
pub fn split(source: &str) -> (Option<&str>, &str) {
    let Some(rest) = strip_fence_line(source, "---") else {
        return (None, source);
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']).trim_end();
        if trimmed == "---" || trimmed == "..." {
            let yaml = &rest[..offset];
            let body = &rest[offset + line.len()..];
            return (Some(yaml), body);
        }
        offset += line.len();
    }
    (None, source)
}

/// If `source` starts with a line consisting only of `fence`, returns what follows that line.
fn strip_fence_line<'a>(source: &'a str, fence: &str) -> Option<&'a str> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let rest = source.strip_prefix(fence)?;
    let rest = rest.trim_start_matches([' ', '\t']);
    rest.strip_prefix("\r\n").or_else(|| rest.strip_prefix('\n'))
}

/// Parses YAML frontmatter into a JSON value. Empty frontmatter becomes an empty object.
pub fn to_json(yaml: Option<&str>) -> Result<Json, serde_yaml_ng::Error> {
    let Some(yaml) = yaml else {
        return Ok(Json::Object(Map::new()));
    };
    let value: Yaml = serde_yaml_ng::from_str(yaml)?;
    Ok(match yaml_to_json(value) {
        Json::Null => Json::Object(Map::new()),
        json => json,
    })
}

fn yaml_to_json(value: Yaml) -> Json {
    match value {
        Yaml::Null => Json::Null,
        Yaml::Bool(b) => Json::Bool(b),
        Yaml::Number(n) => {
            if let Some(i) = n.as_i64() {
                Json::Number(i.into())
            } else if let Some(u) = n.as_u64() {
                Json::Number(u.into())
            } else {
                // NaN and infinities have no JSON representation.
                n.as_f64()
                    .and_then(Number::from_f64)
                    .map_or(Json::Null, Json::Number)
            }
        }
        Yaml::String(s) => Json::String(s),
        Yaml::Sequence(seq) => Json::Array(seq.into_iter().map(yaml_to_json).collect()),
        Yaml::Mapping(map) => Json::Object(
            map.into_iter()
                .map(|(k, v)| (key_to_string(k), yaml_to_json(v)))
                .collect(),
        ),
        Yaml::Tagged(tagged) => yaml_to_json(tagged.value),
    }
}

/// JSON object keys must be strings, while YAML keys can be any value.
fn key_to_string(key: Yaml) -> String {
    match key {
        Yaml::String(s) => s,
        Yaml::Null => "null".to_owned(),
        other => match yaml_to_json(other) {
            Json::String(s) => s,
            json => json.to_string(),
        },
    }
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
