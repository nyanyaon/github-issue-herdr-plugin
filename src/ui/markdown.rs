//! Markdown as readable terminal text.
//!
//! Light styling only, per [ADR-0002]: headings bold, lists bulleted with a
//! hang indent, fenced code dimmed behind a `│` gutter, inline code reversed,
//! links as text with the target dimmed after it. Tables and images degrade to
//! their raw source rather than rendering badly — there is no alignment engine
//! and no terminal graphics protocol here.
//!
//! Rendering is a pure function of the source and the pane's current width, so
//! a `SIGWINCH` is nothing more than the next draw at a new width.
//!
//! [ADR-0002]: ../../docs/adr/0002-pane-ui-shape.md

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// The gutter a fenced code block sits behind.
const CODE_GUTTER: &str = "│ ";

/// Below this the wrapper stops trying and just fills what it has.
const MIN_WIDTH: usize = 4;

/// Renders markdown into lines already wrapped to `width` columns.
pub fn render(source: &str, width: usize) -> Vec<Line<'static>> {
    let mut renderer = Renderer::new(width);
    renderer.run(source);
    renderer.lines
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// A block waiting for its continuation lines before it can be wrapped.
enum Pending {
    None,
    Paragraph(String),
    /// A list item: `prefix` starts its first line, `hang` every line after.
    Item {
        prefix: String,
        hang: String,
        text: String,
    },
}

struct Renderer {
    width: usize,
    lines: Vec<Line<'static>>,
    pending: Pending,
    /// The marker that will close the fence we are inside, if we are inside one.
    fence: Option<String>,
}

impl Renderer {
    fn new(width: usize) -> Self {
        Self {
            width: width.max(MIN_WIDTH),
            lines: Vec::new(),
            pending: Pending::None,
            fence: None,
        }
    }

    fn run(&mut self, source: &str) {
        for raw in source.lines() {
            let line = raw.trim_end().replace('\t', "    ");
            if let Some(marker) = self.fence.clone() {
                if line.trim_start().starts_with(&marker) {
                    self.fence = None;
                } else {
                    self.push_code(&line);
                }
                continue;
            }

            let trimmed = line.trim_start();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                self.flush();
                self.fence = Some(trimmed.chars().take(3).collect());
                continue;
            }
            if trimmed.is_empty() {
                self.flush();
                self.push_blank();
                continue;
            }
            // A table row: raw source, unstyled, rather than a bad rendering.
            if trimmed.starts_with('|') {
                self.flush();
                self.push_raw(&line);
                continue;
            }
            if let Some(text) = heading(trimmed) {
                self.flush();
                self.push_wrapped(
                    &inline(text),
                    "",
                    "",
                    Style::default().add_modifier(Modifier::BOLD),
                );
                continue;
            }
            if is_thematic_break(trimmed) {
                self.flush();
                self.push_rule();
                continue;
            }
            if let Some((prefix, hang, text)) = list_item(&line) {
                self.flush();
                self.pending = Pending::Item { prefix, hang, text };
                continue;
            }
            // Anything else continues the block in progress, so a paragraph
            // split over several source lines wraps as one.
            match &mut self.pending {
                Pending::Paragraph(text) | Pending::Item { text, .. } => {
                    text.push(' ');
                    text.push_str(trimmed);
                }
                Pending::None => self.pending = Pending::Paragraph(trimmed.to_string()),
            }
        }
        self.flush();
        // A trailing blank line would only push the content up under scrolling.
        while matches!(self.lines.last(), Some(line) if is_blank(line)) {
            self.lines.pop();
        }
    }

    fn flush(&mut self) {
        match std::mem::replace(&mut self.pending, Pending::None) {
            Pending::None => {}
            Pending::Paragraph(text) => {
                self.push_wrapped(&inline(&text), "", "", Style::default());
            }
            Pending::Item { prefix, hang, text } => {
                self.push_wrapped(&inline(&text), &prefix, &hang, Style::default());
            }
        }
    }

    fn push_wrapped(&mut self, segments: &[Segment], prefix: &str, hang: &str, base: Style) {
        self.lines
            .extend(wrap(segments, self.width, prefix, hang, base));
    }

    fn push_blank(&mut self) {
        // Never open with a blank, and never stack two.
        if self.lines.is_empty() || matches!(self.lines.last(), Some(line) if is_blank(line)) {
            return;
        }
        self.lines.push(Line::default());
    }

    fn push_code(&mut self, text: &str) {
        let room = self
            .width
            .saturating_sub(CODE_GUTTER.chars().count())
            .max(1);
        for chunk in hard_wrap(text, room) {
            self.lines.push(Line::from(vec![
                Span::styled(CODE_GUTTER, dim()),
                Span::styled(chunk, dim()),
            ]));
        }
    }

    /// Raw source, unstyled and unparsed, broken only so it cannot overflow.
    fn push_raw(&mut self, text: &str) {
        for chunk in hard_wrap(text, self.width) {
            self.lines.push(Line::from(Span::raw(chunk)));
        }
    }

    fn push_rule(&mut self) {
        self.lines
            .push(Line::from(Span::styled("─".repeat(self.width), dim())));
    }
}

fn is_blank(line: &Line<'_>) -> bool {
    line.spans.iter().all(|span| span.content.trim().is_empty())
}

/// `## Heading` to `Heading`. Six levels, all rendered the same way — depth is
/// carried by the surrounding text, not by a size the terminal cannot draw.
fn heading(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|ch| *ch == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    if !rest.starts_with(' ') {
        return None;
    }
    Some(rest.trim())
}

fn is_thematic_break(line: &str) -> bool {
    let stripped: String = line.chars().filter(|ch| !ch.is_whitespace()).collect();
    stripped.len() >= 3
        && (stripped.chars().all(|ch| ch == '-')
            || stripped.chars().all(|ch| ch == '*')
            || stripped.chars().all(|ch| ch == '_'))
}

/// A list item's bullet, its hang indent, and the text after the marker.
///
/// Unordered markers become `•`; an ordered marker keeps its number, because
/// the number is the content.
fn list_item(line: &str) -> Option<(String, String, String)> {
    let indent = line.len() - line.trim_start().len();
    let rest = line.trim_start();
    let (marker, text) = if let Some(text) = rest
        .strip_prefix("- ")
        .or_else(|| rest.strip_prefix("* "))
        .or_else(|| rest.strip_prefix("+ "))
    {
        ("•".to_string(), text)
    } else {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 || digits > 9 {
            return None;
        }
        let after = &rest[digits..];
        let text = after
            .strip_prefix(". ")
            .or_else(|| after.strip_prefix(") "))?;
        (format!("{}.", &rest[..digits]), text)
    };

    // Nesting shows as indentation, halved so a deep list still has room.
    let pad = " ".repeat(indent / 2);
    let prefix = format!("{pad}{marker} ");
    let hang = " ".repeat(prefix.chars().count());
    Some((prefix, hang, text.trim().to_string()))
}

/// A run of text sharing one style.
struct Segment {
    text: String,
    style: Style,
}

/// Splits a line into styled runs: inline code, links, images and bold.
///
/// An image keeps its raw `![alt](url)` source — there is no image protocol
/// here, and the source at least says what is missing.
fn inline(text: &str) -> Vec<Segment> {
    let chars: Vec<char> = text.chars().collect();
    let mut segments: Vec<Segment> = Vec::new();
    let mut plain = String::new();
    let mut index = 0;

    let flush_plain = |plain: &mut String, segments: &mut Vec<Segment>| {
        if !plain.is_empty() {
            segments.push(Segment {
                text: std::mem::take(plain),
                style: Style::default(),
            });
        }
    };

    while index < chars.len() {
        let ch = chars[index];
        if ch == '`'
            && let Some(end) = find(&chars, index + 1, '`')
        {
            flush_plain(&mut plain, &mut segments);
            segments.push(Segment {
                text: chars[index + 1..end].iter().collect(),
                style: Style::default().add_modifier(Modifier::REVERSED),
            });
            index = end + 1;
            continue;
        }
        if ch == '!'
            && chars.get(index + 1) == Some(&'[')
            && let Some((_, _, end)) = link(&chars, index + 1)
        {
            flush_plain(&mut plain, &mut segments);
            segments.push(Segment {
                text: chars[index..=end].iter().collect(),
                style: Style::default(),
            });
            index = end + 1;
            continue;
        }
        if ch == '['
            && let Some((label, target, end)) = link(&chars, index)
        {
            flush_plain(&mut plain, &mut segments);
            segments.push(Segment {
                text: label,
                style: Style::default(),
            });
            segments.push(Segment {
                text: format!(" ({target})"),
                style: dim(),
            });
            index = end + 1;
            continue;
        }
        if ch == '*'
            && chars.get(index + 1) == Some(&'*')
            && let Some(end) = find_pair(&chars, index + 2)
        {
            flush_plain(&mut plain, &mut segments);
            segments.push(Segment {
                text: chars[index + 2..end].iter().collect(),
                style: Style::default().add_modifier(Modifier::BOLD),
            });
            index = end + 2;
            continue;
        }
        plain.push(ch);
        index += 1;
    }
    flush_plain(&mut plain, &mut segments);
    segments
}

fn find(chars: &[char], from: usize, needle: char) -> Option<usize> {
    (from..chars.len()).find(|index| chars[*index] == needle)
}

/// The end of a `**` run.
fn find_pair(chars: &[char], from: usize) -> Option<usize> {
    (from..chars.len().saturating_sub(1))
        .find(|index| chars[*index] == '*' && chars[index + 1] == '*')
}

/// `[label](target)` starting at `open`, returning its label, target and the
/// index of the closing parenthesis.
fn link(chars: &[char], open: usize) -> Option<(String, String, usize)> {
    let close = find(chars, open + 1, ']')?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = find(chars, close + 2, ')')?;
    let label: String = chars[open + 1..close].iter().collect();
    let target: String = chars[close + 2..end].iter().collect();
    if label.is_empty() || target.is_empty() {
        return None;
    }
    Some((label, target, end))
}

/// One word, kept whole across a style boundary so `` `x` ``'s trailing comma
/// never lands on the next line by itself.
#[derive(Default)]
struct Word {
    pieces: Vec<(String, Style)>,
}

impl Word {
    fn width(&self) -> usize {
        self.pieces
            .iter()
            .map(|(text, _)| text.chars().count())
            .sum()
    }

    fn is_empty(&self) -> bool {
        self.width() == 0
    }

    fn push(&mut self, ch: char, style: Style) {
        match self.pieces.last_mut() {
            Some((text, last)) if *last == style => text.push(ch),
            _ => self.pieces.push((ch.to_string(), style)),
        }
    }
}

fn words(segments: &[Segment]) -> Vec<Word> {
    let mut words = Vec::new();
    let mut current = Word::default();
    for segment in segments {
        for ch in segment.text.chars() {
            if ch.is_whitespace() {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            } else {
                current.push(ch, segment.style);
            }
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Breaks a word too long for the pane, so an unbroken URL cannot overflow it.
fn split_word(word: Word, max: usize) -> Vec<Word> {
    if word.width() <= max {
        return vec![word];
    }
    let mut parts = Vec::new();
    let mut current = Word::default();
    for (text, style) in word.pieces {
        for ch in text.chars() {
            if current.width() == max {
                parts.push(std::mem::take(&mut current));
            }
            current.push(ch, style);
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Greedy word wrap to `width`, with `prefix` on the first line and `hang` on
/// every line after it.
fn wrap(
    segments: &[Segment],
    width: usize,
    prefix: &str,
    hang: &str,
    base: Style,
) -> Vec<Line<'static>> {
    let hang_width = hang.chars().count();
    let room = width.saturating_sub(hang_width).max(1);

    let mut lines = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = prefix.chars().count();
    if !prefix.is_empty() {
        spans.push(Span::raw(prefix.to_string()));
    }
    let mut at_start = true;

    for word in words(segments) {
        for part in split_word(word, room) {
            let part_width = part.width();
            if !at_start && used + 1 + part_width > width {
                lines.push(Line::from(std::mem::take(&mut spans)));
                if !hang.is_empty() {
                    spans.push(Span::raw(hang.to_string()));
                }
                used = hang_width;
                at_start = true;
            }
            if !at_start {
                spans.push(Span::raw(" "));
                used += 1;
            }
            for (text, style) in part.pieces {
                used += text.chars().count();
                spans.push(Span::styled(text, base.patch(style)));
            }
            at_start = false;
        }
    }

    if !at_start || !spans.is_empty() {
        lines.push(Line::from(spans));
    }
    lines
}

/// Breaks text on character boundaries — for code and raw source, where there
/// are no words to break on.
fn hard_wrap(text: &str, room: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    text.chars()
        .collect::<Vec<_>>()
        .chunks(room.max(1))
        .map(|chunk| chunk.iter().collect())
        .collect()
}
