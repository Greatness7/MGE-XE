//! Line-oriented parser for the legacy MGE-XE `.ovr` override format.

use std::io;

use atoi::atoi;
use bstr::{ByteSlice, io::BufReadExt};
use smallvec::SmallVec;
use uncased::Uncased;

use crate::mge_xe::distant_statics::StaticType;
use crate::vfs::normalize_mesh_override_key;

use super::{DynamicVisKind, OverridesBuilder, StaticOverride};

/// Active section while parsing an override file.
#[derive(Clone, Copy, PartialEq)]
enum Section {
    /// Default (unnamed) section: mesh-path keyword overrides.
    Default,
    /// `[names]` section: object-name enable/disable overrides.
    Names,
    /// `[interiors]` section: interior-cell enable/disable overrides.
    Interiors,
    /// `[dynamic_vis]` section: dynamic-visibility group definitions.
    DynamicVis,
}

/// Parses non-empty lines from a `BufRead` source into `builder`.
pub(super) fn parse_override_reader<R: io::BufRead>(reader: &mut R, builder: &mut OverridesBuilder) -> io::Result<()> {
    let mut section = Section::Default;

    reader.for_byte_line(|line| {
        let has_escapes = matches!(section, Section::Names | Section::DynamicVis);
        let line = strip_comment(line, has_escapes).trim();

        if line.is_empty() {
            return Ok(true);
        }

        if let Some(inner) = line.strip_prefix(b"[").and_then(|l| l.strip_suffix(b"]")) {
            let inner = inner.trim();
            if inner.eq_ignore_ascii_case(b"names") {
                section = Section::Names;
            } else if inner.eq_ignore_ascii_case(b"interiors") {
                section = Section::Interiors;
            } else if inner.eq_ignore_ascii_case(b"dynamic_vis") {
                section = Section::DynamicVis;
            }
            return Ok(true);
        }

        match section {
            Section::Default => parse_mesh_override(line, builder),
            Section::Names => parse_name_override(line, builder),
            Section::Interiors => parse_interior_override(line, builder),
            Section::DynamicVis => parse_dynamic_vis(line, builder),
        }

        Ok(true)
    })?;

    Ok(())
}

/// Parses one `key = keywords…` line from the default section and inserts the result.
///
/// Uses the rightmost `=` as the separator so mesh paths containing `=` are handled correctly.
fn parse_mesh_override(line: &[u8], builder: &mut OverridesBuilder) {
    let Some(pos) = line.rfind_byte(b'=') else {
        return;
    };
    let key = line[..pos].trim();
    let value = line[pos + 1..].trim();
    if key.is_empty() {
        return;
    }
    let Ok(key) = key.to_str() else { return };
    builder.insert_mesh_override(normalize_mesh_override_key(key).into_owned(), parse_static_keywords(value));
}

/// Parses one line from the `[names]` section.
///
/// Supports `key = enable | disable` and bare `key` (treated as `disable`). Keys are
/// lowercased and `\:` escape sequences are unescaped before insertion.
fn parse_name_override(line: &[u8], builder: &mut OverridesBuilder) {
    if let Some(pos) = line.rfind_byte(b'=') {
        let key = line[..pos].trim();
        let value = line[pos + 1..].trim();
        if key.is_empty() {
            return;
        }
        let mut key = unescape(key);
        key.make_ascii_lowercase();
        if value.eq_ignore_ascii_case(b"enable") {
            builder.insert_name(key, true);
        } else if value.eq_ignore_ascii_case(b"disable") {
            builder.insert_name(key, false);
        }
    } else {
        let mut key = unescape(line);
        key.make_ascii_lowercase();
        if !key.is_empty() {
            builder.insert_name(key, false);
        }
    }
}

/// Parses one line from the `[interiors]` section.
///
/// Supports `key = enable | disable` and bare `key` (treated as `enable`).
fn parse_interior_override(line: &[u8], builder: &mut OverridesBuilder) {
    if let Some(pos) = line.rfind_byte(b'=') {
        let key = line[..pos].trim();
        let value = line[pos + 1..].trim();
        if key.is_empty() {
            return;
        }
        let Ok(key) = key.to_str() else { return };
        if value.eq_ignore_ascii_case(b"enable") {
            builder.insert_interior(key.to_owned().into(), true);
        } else if value.eq_ignore_ascii_case(b"disable") {
            builder.insert_interior(key.to_owned().into(), false);
        }
    } else {
        let Ok(key) = line.to_str() else { return };
        if !key.is_empty() {
            builder.insert_interior(Uncased::from(key.to_owned()), true);
        }
    }
}

/// Parses one line from the `[dynamic_vis]` section and inserts the group.
///
/// Supported formats:
/// - `id = journal <journal_id> [ranges…]`
/// - `id = global <global_id> [ranges…]`
/// - `id = unique_object [linked_ids…]`
///
/// Duplicate groups (same kind, id, and ranges) are merged rather than inserted again.
/// Script and unique-object lookup tables are kept in sync.
fn parse_dynamic_vis(line: &[u8], builder: &mut OverridesBuilder) {
    let Some(pos) = line.find_byte(b'=') else {
        return;
    };
    let mut key = unescape(line[..pos].trim());
    key.make_ascii_lowercase();
    let value = line[pos + 1..].trim();
    if key.is_empty() || value.is_empty() {
        return;
    }

    let tokens: Vec<&[u8]> = value.fields().collect();
    if tokens.is_empty() {
        return;
    }

    let kind = if tokens[0].eq_ignore_ascii_case(b"journal") && tokens.len() >= 2 {
        let Ok(journal_id) = tokens[1].to_str() else {
            return;
        };
        let ranges = parse_ranges(&tokens[2..]);
        DynamicVisKind::Journal {
            journal_id: journal_id.to_ascii_lowercase(),
            ranges,
        }
    } else if tokens[0].eq_ignore_ascii_case(b"global") && tokens.len() >= 2 {
        let Ok(global_id) = tokens[1].to_str() else {
            return;
        };
        let ranges = parse_ranges(&tokens[2..]);
        DynamicVisKind::Global {
            global_id: global_id.to_ascii_lowercase(),
            ranges,
        }
    } else if tokens[0].eq_ignore_ascii_case(b"unique_object") {
        let mut linked_ids = Vec::with_capacity(tokens.len());
        linked_ids.push(key.clone());
        for &tok in &tokens[1..] {
            if let Ok(s) = tok.to_str() {
                linked_ids.push(s.to_ascii_lowercase());
            }
        }
        DynamicVisKind::UniqueObject {
            source_id: key.clone(),
            linked_ids,
        }
    } else {
        return;
    };

    builder.insert_dynamic_vis(key, kind);
}

/// Parses a whitespace-separated sequence of keyword tokens into a [`StaticOverride`].
///
/// Recognized tokens include: `ignore`, `auto`, `near`, `far`, `very_far`, `grass`,
/// `grass_<pct>`, `tree`, `building`, `no_script`, `use_old_reduction`, and `reduction_<pct>`.
/// Unknown tokens are silently skipped for forward compatibility.
pub(super) fn parse_static_keywords(value: &[u8]) -> StaticOverride {
    let mut result = StaticOverride::default();

    for token in value.fields() {
        if token.eq_ignore_ascii_case(b"ignore") {
            result.ignore = true;
        } else if token.eq_ignore_ascii_case(b"auto") {
            result.static_type = StaticType::StaticAuto;
        } else if token.eq_ignore_ascii_case(b"near") {
            result.static_type = StaticType::StaticNear;
        } else if token.eq_ignore_ascii_case(b"far") {
            result.static_type = StaticType::StaticFar;
        } else if token.eq_ignore_ascii_case(b"very_far") {
            result.static_type = StaticType::StaticVeryFar;
        } else if token.eq_ignore_ascii_case(b"grass") {
            result.static_type = StaticType::StaticGrass;
        } else if token.len() > 6 && token[..6].eq_ignore_ascii_case(b"grass_") {
            result.static_type = StaticType::StaticGrass;
            if let Some(pct) = atoi::<u32>(&token[6..]) {
                result.density = pct.min(100) as f32 / 100.0;
            }
        } else if token.eq_ignore_ascii_case(b"tree") {
            result.static_type = StaticType::StaticTree;
        } else if token.eq_ignore_ascii_case(b"building") {
            result.static_type = StaticType::StaticBuilding;
        } else if token.eq_ignore_ascii_case(b"no_script") {
            result.no_script = true;
        } else if token.eq_ignore_ascii_case(b"use_old_reduction") {
            result.simplify = Some(0.0);
        } else if token.len() > 10
            && token[..10].eq_ignore_ascii_case(b"reduction_")
            && let Some(pct) = atoi::<u32>(&token[10..])
            && pct <= 100
        {
            result.simplify = Some(pct as f32 / 100.0);
        }
    }

    result
}

/// Parses up to 8 range tokens from a dynamic-visibility group line.
///
/// Each token is either an inclusive `start-end` pair or a bare integer `n`. The returned
/// tuples use the half-open representation consumed by `usage.data` and the runtime.
pub(super) fn parse_ranges(tokens: &[&[u8]]) -> SmallVec<[(i32, i32); 8]> {
    let mut ranges = SmallVec::new();
    for &tok in tokens {
        if ranges.len() >= 8 {
            break;
        }
        if let Some(pos) = tok.find_byte(b'-') {
            let start: Option<i32> = atoi(&tok[..pos]);
            let end: Option<i32> = atoi(&tok[pos + 1..]);
            if let (Some(s), Some(e)) = (start, end)
                && let Some(end_exclusive) = e.checked_add(1)
            {
                ranges.push((s, end_exclusive));
            }
        } else if let Some(v) = atoi::<i32>(tok)
            && let Some(end_exclusive) = v.checked_add(1)
        {
            ranges.push((v, end_exclusive));
        }
    }
    ranges
}

/// Truncates `line` at the first unescaped `:` comment delimiter.
///
/// When `handle_escapes` is `false` the first `:` ends the content regardless of backslashes.
/// When `handle_escapes` is `true`, `\:` is treated as a literal colon and does not start a
/// comment, matching the behavior used in `[names]` and `[dynamic_vis]` sections.
pub(super) fn strip_comment(line: &[u8], handle_escapes: bool) -> &[u8] {
    if !handle_escapes {
        if let Some(pos) = line.find_byte(b':') {
            return &line[..pos];
        }
        return line;
    }

    // With escape handling: `\:` is a literal colon.
    let mut i = 0;
    while i < line.len() {
        if line[i] == b'\\' {
            i += 2; // skip escaped character
        } else if line[i] == b':' {
            return &line[..i];
        } else {
            i += 1;
        }
    }
    line
}

/// Resolves `\\`-prefixed escape sequences in `input`, returning an owned UTF-8 string.
///
/// `\\c` becomes `c` for any byte `c`. Non-escape bytes are copied verbatim. Invalid UTF-8
/// sequences in the result are replaced with an empty string.
pub(super) fn unescape(input: &[u8]) -> String {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'\\' && i + 1 < input.len() {
            out.push(input[i + 1]);
            i += 2;
        } else {
            out.push(input[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_default()
}
