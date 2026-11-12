/// Replaces controls and invisibles except newlines and tabs, trims, and keeps `max` characters plus an ellipsis.
pub(crate) fn clean(text: &str, max: usize) -> String {
    let text: String = text
        .chars()
        .map(|c| match c {
            '\n' | '\t' => c,
            c if c.is_control() || is_invisible(c) => ' ',
            c => c,
        })
        .collect();
    truncate(text.trim(), max)
}

/// Keeps at most `max_chars` input characters and appends an ellipsis when shortened.
pub(crate) fn truncate(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

/// Returns whether the character is one of the filtered invisible Unicode characters.
pub(crate) fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}' | '\u{061C}' | '\u{115F}'..='\u{1160}' | '\u{180E}'
            | '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}' | '\u{FFA0}' | '\u{FFF9}'..='\u{FFFB}'
            | '\u{1D173}'..='\u{1D17A}' | '\u{E0000}'..='\u{E007F}'
    )
}
