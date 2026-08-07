//! Syntax highlighting for the response and body views.
//!
//! A hand-rolled tokenizer rather than a syntax-definition crate: cielago only
//! ever shows JSON, the occasional XML/HTML error page, and plain text, and
//! `syntect`-class dependencies dwarf the rest of the binary.
//!
//! Highlighting is line-oriented — every token type here (JSON strings
//! included, since they cannot contain a raw newline) starts and ends on one
//! line — so a line can be rendered without scanning the ones before it.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    Json,
    Xml,
    Plain,
}

/// Guess the syntax from the first non-blank character, the same way
/// [`crate::app`] picks a temp-file extension for `$EDITOR`.
pub fn detect(text: &str) -> Syntax {
    match text.trim_start().chars().next() {
        Some('{') | Some('[') => Syntax::Json,
        Some('<') => Syntax::Xml,
        _ => Syntax::Plain,
    }
}

const KEY: Color = Color::Cyan;
const STRING: Color = Color::Green;
const NUMBER: Color = Color::Yellow;
const LITERAL: Color = Color::Magenta;
const PUNCT: Color = Color::DarkGray;
const TAG: Color = Color::Blue;

/// Highlight `text`, one [`Line`] per input line.
///
/// `marks_vars` additionally paints `{{variable}}` placeholders — wanted in the
/// request body, where they are live template syntax, but not in a response,
/// where the same braces are just bytes the server sent.
pub fn highlight(text: &str, marks_vars: bool) -> Vec<Line<'static>> {
    let syntax = detect(text);
    text.split('\n')
        .map(|line| match syntax {
            Syntax::Json => json_line(line, marks_vars),
            Syntax::Xml => xml_line(line, marks_vars),
            Syntax::Plain => {
                let mut spans = Vec::new();
                push_text(&mut spans, line, Style::default(), marks_vars);
                Line::from(spans)
            }
        })
        .collect()
}

// ----- JSON -----

fn json_line(line: &str, marks_vars: bool) -> Line<'static> {
    let chars: Vec<char> = line.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' => {
                let start = i;
                i = end_of_string(&chars, i);
                // A string followed by `:` is an object key.
                let is_key = chars[i..]
                    .iter()
                    .find(|c| !c.is_whitespace())
                    .is_some_and(|c| *c == ':');
                let color = if is_key { KEY } else { STRING };
                let text: String = chars[start..i].iter().collect();
                push_text(&mut spans, &text, Style::default().fg(color), marks_vars);
            }
            '-' | '0'..='9' => {
                let start = i;
                while i < chars.len() && is_number_char(chars[i]) {
                    i += 1;
                }
                spans.push(span(&chars[start..i], NUMBER));
            }
            c if c.is_ascii_alphabetic() => {
                let start = i;
                while i < chars.len() && chars[i].is_ascii_alphanumeric() {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                let color = match word.as_str() {
                    "true" | "false" | "null" => LITERAL,
                    _ => Color::Reset,
                };
                spans.push(Span::styled(word, Style::default().fg(color)));
            }
            '{' | '}' | '[' | ']' | ',' | ':' => {
                let start = i;
                i += 1;
                spans.push(span(&chars[start..i], PUNCT));
            }
            _ => {
                let start = i;
                i += 1;
                spans.push(Span::raw(chars[start..i].iter().collect::<String>()));
            }
        }
    }
    Line::from(spans)
}

/// Index just past the closing quote of the string starting at `i`, or the end
/// of the line for an unterminated one.
fn end_of_string(chars: &[char], i: usize) -> usize {
    let mut i = i + 1;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 2,
            '"' => return (i + 1).min(chars.len()),
            _ => i += 1,
        }
    }
    chars.len()
}

fn is_number_char(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E')
}

// ----- XML / HTML -----

/// Markup is coloured structurally: everything between `<` and `>` is a tag,
/// with its name, attribute names and quoted values distinguished; anything
/// else is text.
fn xml_line(line: &str, marks_vars: bool) -> Line<'static> {
    let chars: Vec<char> = line.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] != '<' {
            let start = i;
            while i < chars.len() && chars[i] != '<' {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            push_text(&mut spans, &text, Style::default(), marks_vars);
            continue;
        }

        // `<` … `>`: opening punctuation plus the tag name, then attributes.
        let start = i;
        i += 1;
        while i < chars.len() && matches!(chars[i], '/' | '!' | '?') {
            i += 1;
        }
        while i < chars.len() && !chars[i].is_whitespace() && !matches!(chars[i], '>' | '/') {
            i += 1;
        }
        spans.push(span(&chars[start..i], TAG));

        while i < chars.len() && chars[i] != '>' {
            match chars[i] {
                '"' | '\'' => {
                    let quote = chars[i];
                    let start = i;
                    i += 1;
                    while i < chars.len() && chars[i] != quote {
                        i += 1;
                    }
                    i = (i + 1).min(chars.len());
                    let text: String = chars[start..i].iter().collect();
                    push_text(&mut spans, &text, Style::default().fg(STRING), marks_vars);
                }
                c if c.is_whitespace() || c == '=' || c == '/' => {
                    let start = i;
                    i += 1;
                    spans.push(span(&chars[start..i], PUNCT));
                }
                _ => {
                    let start = i;
                    while i < chars.len()
                        && !chars[i].is_whitespace()
                        && !"=>/\"'".contains(chars[i])
                    {
                        i += 1;
                    }
                    spans.push(span(&chars[start..i], KEY));
                }
            }
        }
        if i < chars.len() {
            spans.push(span(&chars[i..i + 1], TAG));
            i += 1;
        }
    }
    Line::from(spans)
}

// ----- shared -----

fn span(chars: &[char], color: Color) -> Span<'static> {
    Span::styled(chars.iter().collect::<String>(), Style::default().fg(color))
}

/// Push `text` in `style`, breaking out `{{variable}}` placeholders when
/// `marks_vars` is set so template syntax stands out from literal content.
fn push_text(spans: &mut Vec<Span<'static>>, text: &str, style: Style, marks_vars: bool) {
    if text.is_empty() {
        return;
    }
    if !marks_vars {
        spans.push(Span::styled(text.to_string(), style));
        return;
    }
    let var_style = Style::default()
        .fg(LITERAL)
        .add_modifier(Modifier::BOLD | Modifier::ITALIC);
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let Some(end) = rest[start + 2..].find("}}") else {
            break;
        };
        if start > 0 {
            spans.push(Span::styled(rest[..start].to_string(), style));
        }
        let stop = start + 2 + end + 2;
        spans.push(Span::styled(rest[start..stop].to_string(), var_style));
        rest = &rest[stop..];
    }
    if !rest.is_empty() {
        spans.push(Span::styled(rest.to_string(), style));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (text, fg colour) pairs, for asserting on what a line renders as.
    fn tokens(line: &Line<'static>) -> Vec<(String, Option<Color>)> {
        line.spans
            .iter()
            .map(|s| (s.content.to_string(), s.style.fg))
            .collect()
    }

    fn colored(line: &Line<'static>, text: &str) -> Option<Color> {
        tokens(line)
            .into_iter()
            .find(|(t, _)| t == text)
            .and_then(|(_, c)| c)
    }

    #[test]
    fn detects_syntax_from_first_char() {
        assert_eq!(detect("  {\"a\": 1}"), Syntax::Json);
        assert_eq!(detect("[1]"), Syntax::Json);
        assert_eq!(detect("<html>"), Syntax::Xml);
        assert_eq!(detect("plain words"), Syntax::Plain);
        assert_eq!(detect(""), Syntax::Plain);
    }

    #[test]
    fn json_keys_and_values_differ() {
        let lines = highlight(
            "{\n  \"name\": \"ada\",\n  \"n\": -1.5e3,\n  \"ok\": true\n}",
            false,
        );
        assert_eq!(lines.len(), 5);
        assert_eq!(colored(&lines[1], "\"name\""), Some(KEY));
        assert_eq!(colored(&lines[1], "\"ada\""), Some(STRING));
        assert_eq!(colored(&lines[2], "-1.5e3"), Some(NUMBER));
        assert_eq!(colored(&lines[3], "true"), Some(LITERAL));
        assert_eq!(colored(&lines[0], "{"), Some(PUNCT));
    }

    #[test]
    fn json_strings_keep_escapes_and_colons_inside() {
        let lines = highlight("{\"a\": \"x\\\": y\"}", false);
        assert_eq!(colored(&lines[0], "\"a\""), Some(KEY));
        // The escaped quote must not end the string early, and the `:` inside
        // it must not promote the value to a key.
        assert_eq!(colored(&lines[0], "\"x\\\": y\""), Some(STRING));
    }

    #[test]
    fn unterminated_json_string_does_not_panic() {
        let lines = highlight("{\"a\": \"oops", false);
        assert_eq!(colored(&lines[0], "\"oops"), Some(STRING));
    }

    #[test]
    fn every_character_survives_highlighting() {
        for text in [
            "{\"a\": [1, 2, {\"b\": null}], \"c\": \"é☃\"}",
            "<a href=\"/x\">hi &amp; bye</a>",
            "not markup at all",
        ] {
            let rendered: String = highlight(text, true)
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(rendered, text);
        }
    }

    #[test]
    fn xml_tags_attributes_and_text() {
        let lines = highlight("<a href=\"/x\">hi</a>", false);
        assert_eq!(colored(&lines[0], "<a"), Some(TAG));
        assert_eq!(colored(&lines[0], "href"), Some(KEY));
        assert_eq!(colored(&lines[0], "\"/x\""), Some(STRING));
        assert_eq!(colored(&lines[0], "hi"), None);
        assert_eq!(colored(&lines[0], "</a"), Some(TAG));
    }

    #[test]
    fn variables_are_marked_only_when_asked() {
        let body = "{\"id\": \"{{uuid}}\"}";
        let marked = highlight(body, true);
        assert_eq!(colored(&marked[0], "{{uuid}}"), Some(LITERAL));
        assert_eq!(colored(&marked[0], "\""), Some(STRING));

        let plain = highlight(body, false);
        assert_eq!(colored(&plain[0], "\"{{uuid}}\""), Some(STRING));
    }

    #[test]
    fn unclosed_variable_is_left_alone() {
        let lines = highlight("value {{oops", true);
        assert_eq!(tokens(&lines[0]).len(), 1);
    }
}
