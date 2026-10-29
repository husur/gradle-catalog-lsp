use std::collections::HashMap;
use std::ops::Range;

use toml_edit::{Document, Item, TableLike, Value};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Module {
    pub group: String,
    pub name: String,
}

#[derive(Debug, PartialEq)]
pub struct Entry {
    pub version: String,
    pub span: Range<usize>,
    pub modules: Vec<Module>,
}

enum Version<'a> {
    Inline(&'a Value),
    Ref(&'a str),
}

pub fn parse(text: &str) -> Vec<Entry> {
    let Ok(doc) = Document::parse(text) else {
        return vec![];
    };
    let mut entries = vec![];
    let mut refs: HashMap<&str, Vec<Module>> = HashMap::new();
    for (section, plugin) in [("libraries", false), ("plugins", true)] {
        let Some(table) = doc.get(section).and_then(Item::as_table_like) else {
            continue;
        };
        for (_, item) in table.iter() {
            match declaration(item, plugin) {
                Some((module, Version::Ref(key))) => refs.entry(key).or_default().push(module),
                Some((module, Version::Inline(value))) => {
                    entries.extend(entry(text, value, vec![module]))
                }
                None => {}
            }
        }
    }
    if let Some(versions) = doc.get("versions").and_then(Item::as_table_like) {
        for (key, item) in versions.iter() {
            if let (Some(modules), Some(value)) = (refs.remove(key), item.as_value()) {
                entries.extend(entry(text, value, modules));
            }
        }
    }
    entries
}

fn declaration(item: &Item, plugin: bool) -> Option<(Module, Version<'_>)> {
    if let Some(notation) = item.as_value().filter(|value| value.is_str()) {
        let (coordinates, _) = notation.as_str()?.rsplit_once(':')?;
        return Some((module(coordinates, plugin)?, Version::Inline(notation)));
    }
    let table = item.as_table_like()?;
    let coordinates = if plugin {
        string(table, "id")?.to_string()
    } else if let Some(module) = string(table, "module") {
        module.to_string()
    } else {
        format!("{}:{}", string(table, "group")?, string(table, "name")?)
    };
    let version = table.get("version")?;
    let version = match version.as_table_like() {
        Some(rich) => Version::Ref(string(rich, "ref")?),
        None => Version::Inline(version.as_value()?),
    };
    Some((module(&coordinates, plugin)?, version))
}

fn module(coordinates: &str, plugin: bool) -> Option<Module> {
    if plugin {
        return Some(Module {
            group: coordinates.to_string(),
            name: format!("{coordinates}.gradle.plugin"),
        });
    }
    let (group, name) = coordinates.split_once(':')?;
    Some(Module {
        group: group.to_string(),
        name: name.to_string(),
    })
}

fn string<'a>(table: &'a dyn TableLike, key: &str) -> Option<&'a str> {
    table.get(key)?.as_str()
}

fn entry(text: &str, value: &Value, modules: Vec<Module>) -> Option<Entry> {
    let content = value.as_str()?;
    let span = value.span()?;
    let inner = span.start + 1..span.end - 1;
    if text.get(inner.clone())? != content {
        return None;
    }
    let version_start = content.rfind(':').map_or(0, |colon| colon + 1);
    let version = &content[version_start..];
    let dynamic =
        version.starts_with("latest.") || version.contains(['+', '[', ']', '(', ')', ',']);
    if version.is_empty() || dynamic {
        return None;
    }
    Some(Entry {
        version: version.to_string(),
        span: inner.start + version_start..inner.end,
        modules,
    })
}
