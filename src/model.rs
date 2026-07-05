use indexmap::IndexMap;
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub fn objects_path(config_id: &str) -> PathBuf {
    PathBuf::from(format!("config/{config_id}/objects.json"))
}

pub fn config_path(config_id: &str, override_path: Option<&str>) -> PathBuf {
    override_path
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("config/{config_id}/config.json")))
}

/// Strips `/* */` block comments and `//` line comments outside of string
/// literals. Unlike bootstrap.py's regex-based stripping (which strips `//`
/// anywhere, including inside string values), this respects string
/// boundaries -- a deliberate, strictly-more-correct improvement.
fn strip_json_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    let mut in_string = false;

    while let Some((_, c)) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some((_, next)) = chars.next() {
                    out.push(next);
                }
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }

        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if matches!(chars.peek(), Some((_, '/'))) => {
                chars.next();
                for (_, c2) in chars.by_ref() {
                    if c2 == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if matches!(chars.peek(), Some((_, '*'))) => {
                chars.next();
                let mut prev = ' ';
                for (_, c2) in chars.by_ref() {
                    if prev == '*' && c2 == '/' {
                        break;
                    }
                    prev = c2;
                }
            }
            _ => out.push(c),
        }
    }

    out
}

pub fn load_jsonc<T: for<'de> Deserialize<'de>>(path: &Path) -> anyhow::Result<T> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("reading {}: {}", path.display(), e))?;
    let stripped = strip_json_comments(&raw);
    serde_json::from_str(&stripped).map_err(|e| anyhow::anyhow!("parsing {}: {}", path.display(), e))
}

#[derive(Debug, Deserialize)]
pub struct FlagSet {
    pub base: Option<String>,
    #[serde(default)]
    pub flags: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub progress_categories: IndexMap<String, String>,
    pub compiler: String,
    pub compiler_c: Option<String>,
    pub compiler_root: String,
    pub sdk_root: String,
    pub cflags: IndexMap<String, FlagSet>,
    pub cxxflags: Option<IndexMap<String, FlagSet>>,
}

#[derive(Debug, Deserialize)]
pub struct LibEntry {
    pub progress_category: String,
    pub cflags: String,
    pub idapro: String,
    pub delink: Option<String>,
    pub binary: Option<String>,
    pub objects: IndexMap<String, Option<String>>,
}

pub type Objects = IndexMap<String, LibEntry>;

pub fn load_objects(path: &Path) -> anyhow::Result<Objects> {
    load_jsonc(path)
}

/// Mirrors bootstrap.py's `--only` filtering: keep just the named libraries,
/// warning (not failing) about any name that isn't in objects.json.
pub fn filter_only(objects: Objects, only: &[String]) -> anyhow::Result<Objects> {
    if only.is_empty() {
        return Ok(objects);
    }
    let mut missing: Vec<&str> = only
        .iter()
        .map(String::as_str)
        .filter(|w| !objects.contains_key(*w))
        .collect();
    missing.sort();
    if !missing.is_empty() {
        crate::log::warn(&format!(
            "--only names not in objects.json: {}",
            missing.join(", ")
        ));
    }
    let wanted: std::collections::HashSet<&str> = only.iter().map(String::as_str).collect();
    Ok(objects.into_iter().filter(|(k, _)| wanted.contains(k.as_str())).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_line_and_block_comments() {
        let input = "{\n  // a comment\n  \"a\": 1, /* inline */ \"b\": 2\n}";
        let stripped = strip_json_comments(input);
        let value: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(value["a"], 1);
        assert_eq!(value["b"], 2);
    }

    #[test]
    fn leaves_slashes_inside_strings_alone() {
        let input = r#"{ "url": "http://example.com" }"#;
        let stripped = strip_json_comments(input);
        let value: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(value["url"], "http://example.com");
    }

    #[test]
    fn deserializes_flagset_with_base_inheritance() {
        let json = r#"{
            "decomp": { "flags": ["/O2"] },
            "x86math.dll": { "base": "decomp", "flags": ["/G6"] }
        }"#;
        let map: IndexMap<String, FlagSet> = serde_json::from_str(json).unwrap();
        assert_eq!(map["decomp"].flags, vec!["/O2"]);
        assert_eq!(map["x86math.dll"].base.as_deref(), Some("decomp"));
        assert_eq!(map["x86math.dll"].flags, vec!["/G6"]);
    }

    #[test]
    fn preserves_insertion_order() {
        let json = r#"{"b": 1, "a": 2, "c": 3}"#;
        let map: IndexMap<String, i32> = serde_json::from_str(json).unwrap();
        let keys: Vec<&String> = map.keys().collect();
        assert_eq!(keys, vec!["b", "a", "c"]);
    }

    #[test]
    fn filter_only_keeps_wanted_and_warns_on_missing() {
        let json = r#"{
            "A.dll": {"progress_category": "A.dll", "cflags": "A.dll", "idapro": "x", "objects": {}},
            "B.dll": {"progress_category": "B.dll", "cflags": "B.dll", "idapro": "x", "objects": {}}
        }"#;
        let objects: Objects = serde_json::from_str(json).unwrap();
        let filtered = filter_only(objects, &["A.dll".to_string(), "Z.dll".to_string()]).unwrap();
        assert_eq!(filtered.keys().collect::<Vec<_>>(), vec!["A.dll"]);
    }
}
