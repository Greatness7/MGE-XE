use toml_edit::{InlineTable, Item, RawString, Value};

use super::{max_table_position, renumber_tables, separate_from_preceding_table};

/// Restores fixed-schema entries the destination document omitted.
///
/// The walk follows `template` paths only, so tables outside the schema such as
/// `[generation]` and unknown root tables are never touched. Inserted keys and
/// tables carry the template's comments, while their values come from the
/// serialized `baseline` so entries removed by tolerant validation reappear with
/// the value they load as. Existing entries are left byte-for-byte alone.
///
/// Runs before the baseline/current merge: a key the caller changed during this
/// save is inserted with its template comment first, and the merge then applies
/// the changed value while preserving that decoration.
pub(super) fn complete_missing_entries(
    destination: &mut toml_edit::Table,
    template: &toml_edit::Table,
    baseline: &toml_edit::Table,
) {
    let mut next = max_table_position(destination) + 1;
    complete_missing_tables(destination, template, Some(baseline), &mut next);
}

/// Why a template table path needs no further completion in the destination.
#[derive(Clone, Copy)]
enum DestShape {
    /// Present as a regular or dotted table; recurse into it.
    Table,
    /// Present as an inline value whose subtree misses template entries; expand it.
    ExpandInline,
    /// Present as a complete inline value or as data of an unrelated shape; leave alone.
    Leave,
}

fn complete_missing_tables(
    destination: &mut toml_edit::Table,
    template: &toml_edit::Table,
    baseline: Option<&toml_edit::Table>,
    next: &mut isize,
) {
    for (key, template_item) in template.iter() {
        let Some(template_table) = template_item.as_table() else {
            if !destination.contains_key(key) {
                insert_missing_value(destination, template, key, template_item, baseline.and_then(|b| b.get(key)));
            }
            continue;
        };
        let baseline_table = baseline.and_then(|b| b.get(key)).and_then(Item::as_table);
        let shape = match destination.get(key) {
            None => None,
            Some(item) if item.is_table() => Some(DestShape::Table),
            Some(item) => {
                let incomplete_inline = item
                    .as_value()
                    .and_then(Value::as_inline_table)
                    .is_some_and(|inline| !inline_subtree_is_complete(template_table, inline));
                Some(if incomplete_inline {
                    DestShape::ExpandInline
                } else {
                    DestShape::Leave
                })
            }
        };
        match shape {
            None => {
                let mut subtree = template_table.clone();
                if let Some(baseline_table) = baseline_table {
                    seed_values_from_baseline(&mut subtree, baseline_table);
                }
                separate_from_preceding_table(&mut subtree);
                renumber_tables(&mut subtree, next);
                insert_with_template_decor(destination, template, key, Item::Table(subtree));
            }
            // Dotted keys also parse as tables. Their values render with the full dotted
            // path, where inserted keys keep comments and inserted sub-tables stay valid
            // TOML, so no representation change is needed.
            Some(DestShape::Table) => {
                if let Some(dest_table) = destination.get_mut(key).and_then(Item::as_table_mut) {
                    complete_missing_tables(dest_table, template_table, baseline_table, next);
                }
            }
            Some(DestShape::ExpandInline) => {
                let value = destination.get(key).and_then(Item::as_value).expect("classified above");
                let inline = value
                    .as_inline_table()
                    .expect("classified as an incomplete inline table above");
                let mut expanded = expand_inline_table(inline);
                // Carry the inline value's trailing comment onto the new header; member
                // separators inside the inline table are not header material and stay dropped.
                if let Some(suffix) = value.decor().suffix().and_then(RawString::as_str) {
                    let trimmed = suffix.trim_end_matches(['\r', '\n']);
                    if trimmed.trim_start().starts_with('#') {
                        expanded.decor_mut().set_suffix(trimmed);
                    }
                }
                // Keys promoted to headers keep comment-bearing decoration but drop the
                // whitespace-only trivia inherited from the inline syntax, which would
                // otherwise render inside the header brackets.
                let key_has_comment = destination
                    .get_key_value(key)
                    .and_then(|(existing, _)| existing.leaf_decor().prefix())
                    .and_then(RawString::as_str)
                    .is_some_and(|prefix| prefix.contains('#'));
                if key_has_comment {
                    // The key's comment renders before the header; an empty prefix stops
                    // the encoder from adding its default blank line on top of it.
                    expanded.decor_mut().set_prefix("");
                } else {
                    separate_from_preceding_table(&mut expanded);
                }
                if let Some(mut promoted) = destination.key_mut(key) {
                    let decor = promoted.leaf_decor();
                    let prefix_is_trivia = !decor
                        .prefix()
                        .and_then(RawString::as_str)
                        .is_some_and(|trivia| trivia.contains('#'));
                    let suffix_is_trivia = !decor
                        .suffix()
                        .and_then(RawString::as_str)
                        .is_some_and(|trivia| trivia.contains('#'));
                    if prefix_is_trivia {
                        promoted.leaf_decor_mut().set_prefix("");
                    }
                    if suffix_is_trivia {
                        promoted.leaf_decor_mut().set_suffix("");
                    }
                }
                expanded.set_position(Some(*next));
                *next += 1;
                *destination.get_mut(key).expect("classified above") = Item::Table(expanded);
                if let Some(dest_table) = destination.get_mut(key).and_then(Item::as_table_mut) {
                    complete_missing_tables(dest_table, template_table, baseline_table, next);
                }
            }
            Some(DestShape::Leave) => {}
        }
    }
}

/// Inserts an absent template value, with the baseline's value under the template's decoration.
fn insert_missing_value(
    destination: &mut toml_edit::Table,
    template: &toml_edit::Table,
    key: &str,
    template_item: &Item,
    baseline_item: Option<&Item>,
) {
    let mut item = template_item.clone();
    if let (Some(template_value), Some(baseline_value)) = (item.as_value_mut(), baseline_item.and_then(Item::as_value)) {
        let decor = template_value.decor().clone();
        *template_value = baseline_value.clone();
        *template_value.decor_mut() = decor;
    }
    insert_with_template_decor(destination, template, key, item);
}

/// Inserts `item` under `key`, reusing the template key's decoration so comments follow.
fn insert_with_template_decor(destination: &mut toml_edit::Table, template: &toml_edit::Table, key: &str, item: Item) {
    if let Some((template_key, _)) = template.get_key_value(key) {
        destination.insert_formatted(template_key, item);
    } else {
        destination.insert(key, item);
    }
}

/// Copies each template value's underlying baseline value while keeping template decoration.
fn seed_values_from_baseline(template: &mut toml_edit::Table, baseline: &toml_edit::Table) {
    for (key, item) in template.iter_mut() {
        let Some(baseline_item) = baseline.get(key.get()) else {
            continue;
        };
        match item {
            Item::Value(value) => {
                if let Some(baseline_value) = baseline_item.as_value() {
                    let decor = value.decor().clone();
                    *value = baseline_value.clone();
                    *value.decor_mut() = decor;
                }
            }
            Item::Table(child) => {
                if let Some(baseline_child) = baseline_item.as_table() {
                    seed_values_from_baseline(child, baseline_child);
                }
            }
            _ => {}
        }
    }
}

/// Converts an inline table into a regular table, preserving key spelling and decoration.
fn expand_inline_table(inline: &InlineTable) -> toml_edit::Table {
    let mut table = toml_edit::Table::new();
    for (name, value) in inline.iter() {
        // Re-parse the original key so quoted spellings keep their representation, then
        // re-attach its decoration.
        let key = inline.key(name).map_or_else(
            || toml_edit::Key::new(name),
            |existing| {
                let raw = existing.display_repr().to_string();
                match toml_edit::Key::parse(&raw) {
                    Ok(mut parsed) if !parsed.is_empty() => {
                        let mut key = parsed.remove(0);
                        *key.leaf_decor_mut() = existing.leaf_decor().clone();
                        key
                    }
                    _ => {
                        let mut key = toml_edit::Key::new(name);
                        *key.leaf_decor_mut() = existing.leaf_decor().clone();
                        key
                    }
                }
            },
        );
        table.insert_formatted(&key, Item::Value(value.clone()));
    }
    table
}

/// Whether every template entry exists somewhere in the inline subtree.
///
/// Inline tables cannot hold comments or table headers, so any missing entry
/// anywhere below forces the whole enclosing chain to expand.
fn inline_subtree_is_complete(template: &toml_edit::Table, inline: &InlineTable) -> bool {
    template.iter().all(|(key, template_item)| match template_item.as_table() {
        Some(template_child) => inline
            .get(key)
            .and_then(Value::as_inline_table)
            .is_some_and(|child| inline_subtree_is_complete(template_child, child)),
        None => inline.contains_key(key),
    })
}

pub(super) fn current_value_at_path(current: &toml_edit::Table, segments: &[&str]) -> Option<toml_edit::Value> {
    let (segment, remaining) = segments.split_first()?;
    current_value_from_item(current.get(segment)?, remaining)
}

fn current_value_from_item(item: &Item, segments: &[&str]) -> Option<toml_edit::Value> {
    if segments.is_empty() {
        return item.as_value().cloned();
    }
    if let Some(table) = item.as_table() {
        return current_value_at_path(table, segments);
    }
    current_value_from_inline(item.as_inline_table()?, segments)
}

fn current_value_from_inline(table: &toml_edit::InlineTable, segments: &[&str]) -> Option<toml_edit::Value> {
    let (segment, remaining) = segments.split_first()?;
    let value = table.get(segment)?;
    if remaining.is_empty() {
        Some(value.clone())
    } else {
        current_value_from_inline(value.as_inline_table()?, remaining)
    }
}

pub(super) fn force_value_into_table(destination: &mut toml_edit::Table, segments: &[&str], current: toml_edit::Value) {
    let Some((segment, remaining)) = segments.split_first() else {
        return;
    };
    if !remaining.is_empty() {
        if let Some(table) = destination.get_mut(segment).and_then(Item::as_table_mut) {
            force_value_into_table(table, remaining, current);
            return;
        }
        if let Some(table) = destination.get_mut(segment).and_then(Item::as_inline_table_mut) {
            force_value_into_inline(table, remaining, current);
            return;
        }
        destination.insert(segment, Item::Table(toml_edit::Table::new()));
        if let Some(table) = destination.get_mut(segment).and_then(Item::as_table_mut) {
            force_value_into_table(table, remaining, current);
        }
        return;
    }

    if let Some(destination_item) = destination.get_mut(segment) {
        let decor = destination_item.as_value().map(|value| value.decor().clone());
        *destination_item = Item::Value(current);
        if let (Some(decor), Some(value)) = (decor, destination_item.as_value_mut()) {
            *value.decor_mut() = decor;
        }
    } else {
        destination.insert(segment, Item::Value(current));
    }
}

fn force_value_into_inline(destination: &mut toml_edit::InlineTable, segments: &[&str], current: toml_edit::Value) {
    let Some((segment, remaining)) = segments.split_first() else {
        return;
    };
    if !remaining.is_empty() {
        if !destination.get(segment).is_some_and(|value| value.is_inline_table()) {
            destination.insert(
                (*segment).to_owned(),
                toml_edit::Value::InlineTable(toml_edit::InlineTable::new()),
            );
        }
        if let Some(table) = destination.get_mut(segment).and_then(toml_edit::Value::as_inline_table_mut) {
            force_value_into_inline(table, remaining, current);
        }
        return;
    }

    if let Some(destination_value) = destination.get_mut(segment) {
        let decor = destination_value.decor().clone();
        *destination_value = current;
        *destination_value.decor_mut() = decor;
    } else {
        destination.insert((*segment).to_owned(), current);
    }
}

pub(super) fn merge_changed_tables(
    destination: &mut toml_edit::Table,
    baseline: &toml_edit::Table,
    current: &toml_edit::Table,
) {
    for (key, current_item) in current.iter() {
        let baseline_item = baseline.get(key).unwrap_or(&Item::None);
        if let Some(destination_item) = destination.get_mut(key) {
            merge_changed_item(destination_item, baseline_item, current_item);
        } else if !same_item(baseline_item, current_item) {
            destination.insert(key, current_item.clone());
        }
    }
}

fn merge_changed_item(destination: &mut Item, baseline: &Item, current: &Item) {
    if same_item(baseline, current) {
        return;
    }

    if let (Some(baseline_table), Some(current_table)) = (baseline.as_table(), current.as_table())
        && let Some(destination_table) = destination.as_table_mut()
    {
        merge_changed_tables(destination_table, baseline_table, current_table);
        return;
    }

    if let (Some(baseline_inline), Some(current_inline)) = (baseline.as_inline_table(), current.as_inline_table()) {
        if let Some(destination_table) = destination.as_table_mut() {
            for (key, current_value) in current_inline.iter() {
                let baseline_value = baseline_inline.get(key);
                if baseline_value.is_some_and(|value| same_value(value, current_value)) {
                    continue;
                }
                let current_item = Item::Value(current_value.clone());
                let baseline_item = baseline_value.cloned().map(Item::Value).unwrap_or(Item::None);
                if let Some(destination_item) = destination_table.get_mut(key) {
                    merge_changed_item(destination_item, &baseline_item, &current_item);
                } else {
                    destination_table.insert(key, current_item);
                }
            }
            return;
        }
        if let Some(destination_inline) = destination.as_inline_table_mut() {
            for (key, current_value) in current_inline.iter() {
                if !baseline_inline.get(key).is_some_and(|value| same_value(value, current_value)) {
                    destination_inline.insert(key, current_value.clone());
                }
            }
            return;
        }
    }

    let decor = destination.as_value().map(|value| value.decor().clone());
    *destination = current.clone();
    if let (Some(decor), Some(value)) = (decor, destination.as_value_mut()) {
        *value.decor_mut() = decor;
    }
}

fn same_item(left: &Item, right: &Item) -> bool {
    left.to_string() == right.to_string()
}

fn same_value(left: &toml_edit::Value, right: &toml_edit::Value) -> bool {
    left.to_string() == right.to_string()
}
