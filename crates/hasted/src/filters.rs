//! Preset inbox filters: `filters.json` in the data folder, validated here so every client sees
//! the same rules. A bad file is reported (filter name and key only) and never half-applied.

use std::path::Path;

use haste_proto::FilterView;

pub const FILTERS_FILE: &str = "filters.json";
/// Keys a filter may use besides `name`.
const KEYS: [&str; 4] = ["sender", "subject", "source", "min_priority"];
const DEFAULT_NAME: &str = "all";

#[must_use]
pub fn default_filter() -> FilterView {
    FilterView {
        name: DEFAULT_NAME.to_owned(),
        ..FilterView::default()
    }
}

fn strings(name: &str, key: &str, v: &serde_json::Value) -> Result<Vec<String>, String> {
    let one = |x: &serde_json::Value| {
        x.as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("filter \"{name}\": \"{key}\" must be text or a list of text"))
    };
    match v {
        serde_json::Value::Array(items) => items.iter().map(one).collect(),
        other => Ok(vec![one(other)?]),
    }
}

/// Parse the text of `filters.json`.
///
/// # Errors
/// A message naming the filter and key at fault.
pub fn parse(text: &str) -> Result<Vec<FilterView>, String> {
    let root: serde_json::Value =
        serde_json::from_str(text).map_err(|_| "filters.json is not valid JSON".to_owned())?;
    let obj = root
        .as_object()
        .ok_or_else(|| "filters.json must be an object with a \"filters\" list".to_owned())?;
    if let Some(k) = obj.keys().find(|k| *k != "filters") {
        return Err(format!("filters.json: unknown key \"{k}\""));
    }
    let list = obj
        .get("filters")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "filters.json needs a \"filters\" list".to_owned())?;
    let mut out = Vec::new();
    for (i, item) in list.iter().enumerate() {
        let f = item
            .as_object()
            .ok_or_else(|| format!("filter #{}: must be an object", i + 1))?;
        let name = f
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|n| !n.trim().is_empty())
            .ok_or_else(|| format!("filter #{}: needs a \"name\"", i + 1))?
            .to_owned();
        let mut view = FilterView {
            name: name.clone(),
            ..FilterView::default()
        };
        for (key, value) in f {
            match key.as_str() {
                "name" => {}
                "sender" => view.sender = strings(&name, key, value)?,
                "subject" => view.subject = strings(&name, key, value)?,
                "source" => view.source = strings(&name, key, value)?,
                "min_priority" => {
                    view.min_priority = Some(
                        value
                            .as_i64()
                            .and_then(|n| i32::try_from(n).ok())
                            .ok_or_else(|| {
                                format!("filter \"{name}\": \"min_priority\" must be a number")
                            })?,
                    );
                }
                other => {
                    return Err(format!(
                        "filter \"{name}\": unknown key \"{other}\" (allowed: {})",
                        KEYS.join(", ")
                    ))
                }
            }
        }
        out.push(view);
    }
    Ok(out)
}

/// The filters to offer and, when the file was rejected, why. A missing or empty file gives the
/// built-in default only.
#[must_use]
pub fn load(data_dir: &Path) -> (Vec<FilterView>, Option<String>) {
    match std::fs::read_to_string(data_dir.join(FILTERS_FILE)) {
        Err(_) => (vec![default_filter()], None),
        Ok(text) => match parse(&text) {
            Ok(list) if list.is_empty() => (vec![default_filter()], None),
            Ok(list) => (list, None),
            Err(e) => (vec![default_filter()], Some(e)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_valid_file_loads_in_order() {
        let list = parse(
            r#"{"filters":[{"name":"VIP","sender":["boss@x.com"],"min_priority":1},
                          {"name":"Chats","source":"whatsapp"}]}"#,
        )
        .unwrap();
        assert_eq!(list[0].name, "VIP");
        assert_eq!(list[0].min_priority, Some(1));
        assert_eq!(list[1].source, vec!["whatsapp".to_owned()]);
    }

    #[test]
    fn an_unknown_key_names_the_filter_and_the_key() {
        let e = parse(r#"{"filters":[{"name":"VIP","colour":"red"}]}"#).unwrap_err();
        assert!(e.contains("VIP") && e.contains("colour"), "{e}");
    }

    #[test]
    fn missing_or_empty_gives_only_the_default() {
        let dir = std::env::temp_dir().join(format!("haste-filters-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (list, err) = load(&dir);
        assert_eq!(list, vec![default_filter()]);
        assert!(err.is_none());
        std::fs::write(dir.join(FILTERS_FILE), r#"{"filters":[]}"#).unwrap();
        assert_eq!(load(&dir).0, vec![default_filter()]);
        std::fs::write(
            dir.join(FILTERS_FILE),
            r#"{"filters":[{"name":"a","x":1}]}"#,
        )
        .unwrap();
        let (list, err) = load(&dir);
        assert_eq!(list, vec![default_filter()]);
        assert!(err.unwrap().contains("\"x\""));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
