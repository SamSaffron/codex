use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Span as RtSpan;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::FontStyle;
use syntect::highlighting::Style as SyntectStyle;
use syntect::highlighting::Theme;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxReference;
use syntect::parsing::SyntaxSet;

use crate::color::is_light;
use crate::terminal_palette::best_color;
use crate::terminal_palette::default_bg;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DiffLineType {
    Insert,
    Delete,
    Context,
}

pub(crate) struct SyntaxSpan {
    pub(crate) style: SyntectStyle,
    pub(crate) text: String,
}

pub(crate) struct SyntaxHighlighter {
    syntax_set: SyntaxSet,
    theme: Theme,
    plain_style: SyntectStyle,
    syntax_by_extension: Mutex<HashMap<String, Option<usize>>>,
}

static SYNTAX_HIGHLIGHTER: OnceLock<SyntaxHighlighter> = OnceLock::new();

fn syntax_highlighter() -> &'static SyntaxHighlighter {
    SYNTAX_HIGHLIGHTER.get_or_init(SyntaxHighlighter::new)
}

impl SyntaxHighlighter {
    fn new() -> Self {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme_set = ThemeSet::load_defaults();
        let theme = select_theme(&theme_set);
        let plain_style = theme_plain_style(&theme);
        Self {
            syntax_set,
            theme,
            plain_style,
            syntax_by_extension: Mutex::new(HashMap::new()),
        }
    }

    fn highlight_line(&self, text: &str, extension: Option<&str>) -> Vec<SyntaxSpan> {
        if text.is_empty() {
            return Vec::new();
        }

        let syntax = extension
            .and_then(|ext| self.syntax_for_extension(ext))
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());
        let mut highlighter = HighlightLines::new(syntax, &self.theme);
        match highlighter.highlight_line(text, &self.syntax_set) {
            Ok(ranges) => ranges
                .into_iter()
                .map(|(style, segment)| SyntaxSpan {
                    style,
                    text: segment.to_string(),
                })
                .collect(),
            Err(_) => vec![SyntaxSpan {
                style: self.plain_style,
                text: text.to_string(),
            }],
        }
    }

    fn syntax_for_extension(&self, extension: &str) -> Option<&SyntaxReference> {
        let ext = extension.trim_start_matches('.').to_ascii_lowercase();
        if ext.is_empty() {
            return None;
        }

        if let Ok(cache) = self.syntax_by_extension.lock()
            && let Some(index) = cache.get(&ext)
        {
            return index
                .as_ref()
                .and_then(|idx| self.syntax_set.syntaxes().get(*idx));
        }

        let syntax = extension_to_syntax(&ext)
            .and_then(|name| self.syntax_set.find_syntax_by_name(name))
            .or_else(|| self.syntax_set.find_syntax_by_extension(&ext));
        let index = syntax.and_then(|syntax| {
            self.syntax_set
                .syntaxes()
                .iter()
                .position(|candidate| std::ptr::eq(candidate, syntax))
        });
        if let Ok(mut cache) = self.syntax_by_extension.lock() {
            cache.insert(ext, index);
        }
        index.and_then(|idx| self.syntax_set.syntaxes().get(idx))
    }
}

pub(crate) fn highlight_line(text: &str, extension: Option<&str>) -> Vec<SyntaxSpan> {
    syntax_highlighter().highlight_line(text, extension)
}

pub(crate) fn merge_diff_and_syntax_styles(
    diff_type: DiffLineType,
    syntax_spans: Vec<SyntaxSpan>,
) -> Vec<RtSpan<'static>> {
    let background = diff_background(diff_type);
    syntax_spans
        .into_iter()
        .map(|span| {
            let style = syntect_to_ratatui(span.style, background);
            RtSpan::styled(span.text, style)
        })
        .collect()
}

pub(crate) fn wrap_syntax_spans(
    syntax_spans: Vec<SyntaxSpan>,
    width: usize,
) -> Vec<Vec<SyntaxSpan>> {
    let width = width.max(1);
    if syntax_spans.is_empty() {
        return vec![Vec::new()];
    }

    let mut lines: Vec<Vec<SyntaxSpan>> = Vec::new();
    let mut current: Vec<SyntaxSpan> = Vec::new();
    let mut current_len = 0;

    for SyntaxSpan { style, text } in syntax_spans {
        let mut remaining = text.as_str();
        while !remaining.is_empty() {
            if current_len == width {
                lines.push(std::mem::take(&mut current));
                current_len = 0;
            }
            let remaining_cols = width.saturating_sub(current_len).max(1);
            let (chunk, rest) = split_at_char_boundary(remaining, remaining_cols);
            if !chunk.is_empty() {
                current.push(SyntaxSpan {
                    style,
                    text: chunk.to_string(),
                });
                current_len += chunk.chars().count();
            }
            remaining = rest;
        }
    }

    if current.is_empty() {
        if lines.is_empty() {
            lines.push(Vec::new());
        }
    } else {
        lines.push(current);
    }

    lines
}

pub(crate) fn diff_background(diff_type: DiffLineType) -> Option<Color> {
    let (r, g, b) = match diff_type {
        DiffLineType::Insert => (0, 40, 0),
        DiffLineType::Delete => (40, 0, 0),
        DiffLineType::Context => return None,
    };
    let color_level = supports_color::on_cached(supports_color::Stream::Stdout)?;
    if color_level.has_16m || color_level.has_256 {
        Some(best_color((r, g, b)))
    } else {
        Some(match diff_type {
            DiffLineType::Insert => Color::Green,
            DiffLineType::Delete => Color::Red,
            DiffLineType::Context => return None,
        })
    }
}

fn syntect_to_ratatui(style: SyntectStyle, background: Option<Color>) -> Style {
    let mut merged_style = Style::default().fg(best_color((
        style.foreground.r,
        style.foreground.g,
        style.foreground.b,
    )));
    if let Some(background) = background {
        merged_style = merged_style.bg(background);
    }
    let mut modifiers = Modifier::empty();
    if style.font_style.contains(FontStyle::BOLD) {
        modifiers |= Modifier::BOLD;
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        modifiers |= Modifier::ITALIC;
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        modifiers |= Modifier::UNDERLINED;
    }
    merged_style.add_modifier(modifiers)
}

fn theme_plain_style(theme: &Theme) -> SyntectStyle {
    let mut style = SyntectStyle::default();
    if let Some(foreground) = theme.settings.foreground {
        style.foreground = foreground;
    }
    if let Some(background) = theme.settings.background {
        style.background = background;
    }
    style
}

fn select_theme(theme_set: &ThemeSet) -> Theme {
    let preferred = default_bg()
        .filter(|bg| is_light(*bg))
        .map(|_| "base16-ocean.light")
        .unwrap_or("base16-ocean.dark");
    match theme_set
        .themes
        .get(preferred)
        .or_else(|| theme_set.themes.get("base16-ocean.dark"))
        .or_else(|| theme_set.themes.get("base16-ocean.light"))
        .or_else(|| theme_set.themes.values().next())
    {
        Some(theme) => theme.clone(),
        None => Theme::default(),
    }
}

fn extension_to_syntax(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("Rust"),
        "py" => Some("Python"),
        "js" => Some("JavaScript"),
        "ts" => Some("TypeScript"),
        "go" => Some("Go"),
        "json" => Some("JSON"),
        "yaml" | "yml" => Some("YAML"),
        "toml" => Some("TOML"),
        "sh" | "bash" => Some("Bourne Again Shell (bash)"),
        "c" | "h" => Some("C"),
        "cpp" | "cc" | "cxx" | "hpp" => Some("C++"),
        "java" => Some("Java"),
        "html" | "htm" => Some("HTML"),
        "css" => Some("CSS"),
        "md" | "markdown" => Some("Markdown"),
        "sql" => Some("SQL"),
        "rb" => Some("Ruby"),
        "php" => Some("PHP"),
        _ => None,
    }
}

fn split_at_char_boundary(text: &str, max_chars: usize) -> (&str, &str) {
    let split_at_byte = text
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or_else(|| text.len());
    text.split_at(split_at_byte)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use ratatui::style::Modifier;
    use syntect::highlighting::Color as SyntectColor;

    #[test]
    fn extension_to_syntax_maps_known_values() {
        assert_eq!(extension_to_syntax("rs"), Some("Rust"));
        assert_eq!(extension_to_syntax("yaml"), Some("YAML"));
        assert_eq!(extension_to_syntax("unknown"), None);
    }

    #[test]
    fn highlight_line_reconstructs_text() {
        let text = "fn main() { let value = 42; }";
        let spans = highlight_line(text, Some("rs"));
        let reconstructed = spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
        assert_eq!(reconstructed, text);
    }

    #[test]
    fn merge_diff_and_syntax_styles_applies_modifiers_and_background() {
        let style = SyntectStyle {
            foreground: SyntectColor {
                r: 10,
                g: 20,
                b: 30,
                a: 0xFF,
            },
            background: SyntectColor {
                r: 0,
                g: 0,
                b: 0,
                a: 0xFF,
            },
            font_style: FontStyle::BOLD | FontStyle::ITALIC,
        };
        let spans = vec![SyntaxSpan {
            style,
            text: "let".to_string(),
        }];
        let merged = merge_diff_and_syntax_styles(DiffLineType::Insert, spans);

        assert_eq!(merged.len(), 1);
        let span = &merged[0];
        assert_eq!(span.content.as_ref(), "let");
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
        assert!(span.style.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(span.style.bg, diff_background(DiffLineType::Insert));
    }
}
