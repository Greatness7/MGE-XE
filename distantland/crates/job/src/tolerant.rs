use serde::de::DeserializeOwned;
use serde_ignored::Path as IgnoredPath;
use serde_path_to_error::Segment as ErrorSegment;
use toml_edit::{DocumentMut, Item, Table, Value};

// Keep the domain-neutral traversal and removal helpers in sync with
// `mge-config/src/document/tolerant.rs`; the caller-specific entry points intentionally differ.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum PathSegment {
    Key(String),
    Index(usize),
}

pub(super) fn deserialize<T: DeserializeOwned>(
    document: &DocumentMut,
) -> Result<T, serde_path_to_error::Error<toml_edit::de::Error>> {
    serde_path_to_error::deserialize(toml_edit::de::Deserializer::from(document.clone()))
}

pub(super) fn deserialize_ignored<T: DeserializeOwned>(
    document: &DocumentMut,
) -> Result<(T, Vec<Vec<PathSegment>>), toml_edit::de::Error> {
    let mut ignored = Vec::new();
    let value = serde_ignored::deserialize(toml_edit::de::Deserializer::from(document.clone()), |path| {
        ignored.push(ignored_segments(&path))
    })?;
    Ok((value, ignored))
}

pub(super) fn error_segments(path: &serde_path_to_error::Path) -> Vec<PathSegment> {
    let mut segments = Vec::new();
    for segment in path {
        match segment {
            ErrorSegment::Map { key } => segments.push(PathSegment::Key(key.clone())),
            ErrorSegment::Seq { index } => segments.push(PathSegment::Index(*index)),
            ErrorSegment::Enum { variant } => segments.push(PathSegment::Key(variant.clone())),
            ErrorSegment::Unknown => break,
        }
    }
    segments
}

pub(super) fn dotted_segments(path: &str) -> Vec<PathSegment> {
    let mut segments = Vec::new();
    for part in path.split('.') {
        if let Some((key, suffix)) = part.split_once('[') {
            if !key.is_empty() {
                segments.push(PathSegment::Key(key.into()));
            }
            let mut rest = suffix;
            while let Some((index, tail)) = rest.split_once(']') {
                if let Ok(index) = index.parse() {
                    segments.push(PathSegment::Index(index));
                }
                let Some(next) = tail.strip_prefix('[') else {
                    break;
                };
                rest = next;
            }
        } else if !part.is_empty() {
            segments.push(PathSegment::Key(part.into()));
        }
    }
    segments
}

pub(super) fn display_path(path: &[PathSegment]) -> String {
    let mut rendered = String::new();
    for segment in path {
        match segment {
            PathSegment::Key(key) => {
                if !rendered.is_empty() {
                    rendered.push('.');
                }
                rendered.push_str(key);
            }
            PathSegment::Index(index) => rendered.push_str(&format!("[{index}]")),
        }
    }
    rendered
}

pub(super) fn remove_bad_value(document: &mut DocumentMut, path: &[PathSegment]) -> bool {
    let end = path
        .iter()
        .rposition(|segment| matches!(segment, PathSegment::Index(_)))
        .map_or(path.len(), |index| index + 1);
    remove_from_table(document.as_table_mut(), &path[..end])
}

fn ignored_segments(path: &IgnoredPath<'_>) -> Vec<PathSegment> {
    match path {
        IgnoredPath::Root => Vec::new(),
        IgnoredPath::Seq { parent, index } => {
            let mut segments = ignored_segments(parent);
            segments.push(PathSegment::Index(*index));
            segments
        }
        IgnoredPath::Map { parent, key } => {
            let mut segments = ignored_segments(parent);
            segments.push(PathSegment::Key(key.clone()));
            segments
        }
        IgnoredPath::Some { parent } | IgnoredPath::NewtypeStruct { parent } | IgnoredPath::NewtypeVariant { parent } => {
            ignored_segments(parent)
        }
    }
}

fn remove_from_table(table: &mut Table, path: &[PathSegment]) -> bool {
    let Some(PathSegment::Key(key)) = path.first() else {
        return false;
    };
    if path.len() == 1 {
        return table.remove(key).is_some();
    }
    table.get_mut(key).is_some_and(|item| remove_from_item(item, &path[1..]))
}

fn remove_from_item(item: &mut Item, path: &[PathSegment]) -> bool {
    match item {
        Item::Table(table) => remove_from_table(table, path),
        Item::ArrayOfTables(array) => {
            let Some(PathSegment::Index(index)) = path.first() else {
                return false;
            };
            if path.len() == 1 {
                return (*index < array.len()).then(|| array.remove(*index)).is_some();
            }
            array
                .get_mut(*index)
                .is_some_and(|table| remove_from_table(table, &path[1..]))
        }
        Item::Value(value) => remove_from_value(value, path),
        Item::None => false,
    }
}

fn remove_from_value(value: &mut Value, path: &[PathSegment]) -> bool {
    match value {
        Value::Array(array) => {
            let Some(PathSegment::Index(index)) = path.first() else {
                return false;
            };
            if path.len() == 1 {
                return (*index < array.len()).then(|| array.remove(*index)).is_some();
            }
            array
                .get_mut(*index)
                .is_some_and(|value| remove_from_value(value, &path[1..]))
        }
        Value::InlineTable(table) => {
            let Some(PathSegment::Key(key)) = path.first() else {
                return false;
            };
            if path.len() == 1 {
                return table.remove(key).is_some();
            }
            table.get_mut(key).is_some_and(|value| remove_from_value(value, &path[1..]))
        }
        _ => false,
    }
}
