//! mwscript `SCTX` scanning for the disable forms distant land cares about.
//!
//! Two mechanisms can hide an object at runtime, and they have different blast radii:
//!
//! - a bare `Disable` in the script *attached* to an object runs on every instance of it when the
//!   instance's cell loads;
//! - `SomeId->Disable` in any script reaches exactly one reference, and only if that reference was
//!   serialized into a plugin's persistent block so Morrowind's records handler can resolve it.
//!
//! Keeping the two classifications separate limits each rule to the references it can affect.

/// What one script's text says about disabling.
#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct ScriptFacts {
    /// Whether the text contains a bare whole-word `Disable`, i.e. one that is not the right-hand
    /// side of `->`.
    ///
    /// The narrowing matters: 252 scripts in the measured load order contain a whole-word
    /// `Disable` *only* as an arrow target, which says nothing about the object the script is
    /// attached to. Treating those as self-disabling wrongly removed visible geometry.
    pub(super) disables_self: bool,
    /// Lowercased ids appearing as `id->Disable` targets, deduplicated, in first-seen order.
    pub(super) disable_targets: Vec<String>,
}

/// Returns whether `byte` continues an identifier, for whole-word matching.
const fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Returns the `->` target immediately left of the `disable` keyword starting at `keyword`.
///
/// `None` means the keyword is a bare statement rather than a member call. This is the signal that
/// separates a self-disable from a remote one. Both the quoted (`"an id"->Disable`) and bare
/// (`an_id->Disable`) forms are accepted, and the arrow may carry whitespace on either side.
///
/// Returns bytes rather than `&str` because `line` is sliced at positions found by matching ASCII
/// delimiters, which are not guaranteed to be UTF-8 char boundaries.
fn disable_target_before(line: &[u8], keyword: usize) -> Option<&[u8]> {
    let mut cursor = keyword;
    while cursor > 0 && line[cursor - 1].is_ascii_whitespace() {
        cursor -= 1;
    }
    // Require a literal `->` arrow; anything else is a bare `Disable` statement.
    if cursor < 2 || line[cursor - 1] != b'>' || line[cursor - 2] != b'-' {
        return None;
    }

    let mut end = cursor - 2;
    while end > 0 && line[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    if end == 0 {
        return None;
    }

    // Quoted target: scan back to the opening quote so ids containing spaces survive.
    let start = if line[end - 1] == b'"' {
        let close = end - 1;
        let open = line[..close].iter().rposition(|&byte| byte == b'"')?;
        end = close;
        open + 1
    } else {
        let mut start = end;
        while start > 0 && (is_word_byte(line[start - 1]) || line[start - 1] == b'\'') {
            start -= 1;
        }
        start
    };

    // An empty target is not a resolvable id, so the keyword is treated as a bare statement.
    (start != end).then(|| &line[start..end])
}

/// Scans one script's `SCTX` text.
///
/// Takes ownership so ASCII case normalization can reuse the parser's text allocation.
///
/// Comments are stripped at the first `;` on each line. A `;` inside a quoted string truncates
/// that line early; mwscript has no string escapes and the pattern is rare enough not to warrant a
/// tokenizer.
pub(super) fn scan_script(mut text: String) -> ScriptFacts {
    text.make_ascii_lowercase();
    let mut facts = ScriptFacts::default();

    for raw_line in text.lines() {
        let line = raw_line.split(';').next().unwrap_or_default();
        let bytes = line.as_bytes();
        let mut cursor = 0;
        while let Some(offset) = line[cursor..].find("disable") {
            let keyword = cursor + offset;
            let end = keyword + "disable".len();
            cursor = end;

            // Whole-word only: `xDisable` and `disabled` are different keywords, and the old
            // substring test matched both, plus any mention in a comment.
            let left_ok = keyword == 0 || !is_word_byte(bytes[keyword - 1]);
            let right_ok = end >= bytes.len() || !is_word_byte(bytes[end]);
            if !left_ok || !right_ok {
                continue;
            }

            match disable_target_before(bytes, keyword) {
                // `line` is a slice of an already-lowercased string, so the target needs no
                // further normalization; the lossy conversion only guards the byte slicing.
                Some(target) => {
                    let target = String::from_utf8_lossy(target).into_owned();
                    if !facts.disable_targets.contains(&target) {
                        facts.disable_targets.push(target);
                    }
                }
                None => facts.disables_self = true,
            }
        }
    }

    facts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disables_self(text: &str) -> bool {
        scan_script(text.to_owned()).disables_self
    }

    fn targets(text: &str) -> Vec<String> {
        scan_script(text.to_owned()).disable_targets
    }

    #[test]
    fn arrow_targets_are_extracted_lowercased_and_deduplicated() {
        assert_eq!(targets("Thing->Disable"), ["thing"]);
        assert_eq!(targets("\"an id\"->Disable"), ["an id"]);
        assert_eq!(targets("thing -> disable"), ["thing"]);
        assert_eq!(targets("dagoth_ur's_thing->Disable"), ["dagoth_ur's_thing"]);
        // Order is first-seen, and repeats collapse, so the list is deterministic per script.
        assert_eq!(targets("b->Disable\na->Disable\nb->Disable"), ["b", "a"]);
        // A bare statement contributes nothing to the target list.
        assert_eq!(targets("Disable"), Vec::<String>::new());
        assert_eq!(targets("; thing->Disable"), Vec::<String>::new());
    }

    #[test]
    fn bare_disable_is_a_self_disable() {
        assert!(disables_self("begin s\n  Disable\nend"));
        assert!(disables_self("disable"));
        assert!(disables_self("\tDISABLE\t"));
        assert!(disables_self("if ( x == 1 )\n  Disable\nendif"));
    }

    #[test]
    fn substring_matches_are_rejected() {
        // The three shapes the old `contains("disable")` test wrongly matched.
        assert!(!disables_self("xDisable"));
        assert!(!disables_self("set disabled to 1"));
        assert!(!disables_self("; this script used to disable the door"));
        assert!(!disables_self("short disable_count"));
    }

    #[test]
    fn arrow_targets_are_not_self_disables() {
        assert!(!disables_self("thing->Disable"));
        assert!(!disables_self("\"an id\"->Disable"));
        assert!(!disables_self("thing -> disable"));
        assert!(!disables_self("thing->\tDisable"));
        // An apostrophe is a legal id character in Morrowind ids.
        assert!(!disables_self("dagoth_ur's_thing->Disable"));
    }

    #[test]
    fn a_script_can_be_both() {
        assert!(disables_self("other->Disable\nDisable"));
    }

    #[test]
    fn a_dangling_arrow_is_treated_as_bare() {
        // Nothing to the left of `->` is not a resolvable target, so it cannot be a remote call.
        assert!(disables_self("->Disable"));
        assert!(disables_self("\"\"->Disable"));
    }

    #[test]
    fn comments_are_stripped_per_line() {
        assert!(!disables_self("thing->Disable ; Disable"));
        assert!(disables_self("; nothing\nDisable ; trailing note"));
    }

    #[test]
    fn non_ascii_text_is_scanned_without_panicking() {
        // Byte offsets come from ASCII delimiter matches, so multi-byte text must not slice badly.
        assert!(disables_self("short \u{e9}\u{e9}\u{e9}\nDisable"));
        assert!(!disables_self("caf\u{e9}_door->Disable"));
        // Accepted imprecision: an id with no ASCII word bytes at its tail reads as an empty
        // target, so the keyword falls back to "bare".
        // Morrowind ids are ASCII in practice; the point here is that it does not panic.
        assert!(disables_self("\u{e9}\u{e9}\u{e9}->Disable"));
    }
}
