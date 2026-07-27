//! The pane's chrome. One column, one status line, never a modal.

pub mod list;
pub mod status;

/// Truncates to `width` columns with an ellipsis, the way a list row's title is
/// clipped. Counts characters, which is exact for the ASCII-dominated text a
/// row carries.
pub fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(width.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}

/// Truncates, then pads to exactly `width` columns.
pub fn fit(text: &str, width: usize) -> String {
    let clipped = truncate(text, width);
    let padding = width.saturating_sub(clipped.chars().count());
    format!("{clipped}{}", " ".repeat(padding))
}

/// Right-aligns within `width` columns.
pub fn right(text: &str, width: usize) -> String {
    let clipped = truncate(text, width);
    let padding = width.saturating_sub(clipped.chars().count());
    format!("{}{clipped}", " ".repeat(padding))
}
