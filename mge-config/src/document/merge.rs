use toml_edit::Item;

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
