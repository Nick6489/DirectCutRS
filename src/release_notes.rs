use std::fmt::Write;

use freshen::ReleaseNotes;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

pub fn format_history(history: &[ReleaseNotes]) -> String {
    let mut text = String::new();
    for release in history.iter().rev() {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        let _ = writeln!(
            text,
            "Version {}\n",
            super::display_version(&release.version.to_string())
        );
        let notes = markdown_text(&release.notes);
        if notes.trim().is_empty() {
            text.push_str("No release notes were provided.");
        } else {
            text.push_str(notes.trim());
        }
    }
    if text.is_empty() {
        text.push_str("No release notes were provided.");
    }
    text.replace('\n', "\r\n")
}

fn markdown_text(markdown: &str) -> String {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut text = String::new();
    let mut lists = Vec::new();
    let mut table_cell = false;
    for event in Parser::new_ext(markdown, options) {
        match event {
            Event::Text(value) | Event::Code(value) => text.push_str(&value),
            Event::SoftBreak => text.push(' '),
            Event::HardBreak => end_line(&mut text),
            Event::Start(Tag::List(start)) => {
                end_line(&mut text);
                lists.push(start);
            }
            Event::Start(Tag::Item) => {
                end_line(&mut text);
                for _ in 1..lists.len() {
                    text.push_str("  ");
                }
                if let Some(Some(number)) = lists.last_mut() {
                    let _ = write!(text, "{number}. ");
                    *number = number.saturating_add(1);
                } else {
                    text.push_str("• ");
                }
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
                if lists.is_empty() {
                    end_block(&mut text);
                }
            }
            Event::End(TagEnd::Item) => end_line(&mut text),
            Event::End(TagEnd::Paragraph) if !lists.is_empty() => end_line(&mut text),
            Event::Start(Tag::CodeBlock(_)) | Event::Rule => end_block(&mut text),
            Event::End(
                TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock | TagEnd::Table,
            ) => {
                end_block(&mut text);
            }
            Event::Start(Tag::TableHead | Tag::TableRow) => table_cell = false,
            Event::Start(Tag::TableCell) => {
                if table_cell {
                    text.push_str(" | ");
                }
                table_cell = true;
            }
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => end_line(&mut text),
            Event::TaskListMarker(checked) => {
                text.push_str(if checked { "[Done] " } else { "[To do] " });
            }
            _ => {}
        }
    }
    text
}

fn end_line(text: &mut String) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
}

fn end_block(text: &mut String) {
    end_line(text);
    if !text.is_empty() && !text.ends_with("\n\n") {
        text.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_history_is_newest_first_and_uses_readable_text() {
        let history = [
            ReleaseNotes { version: "1.1.0".parse().unwrap(), notes: String::new() },
            ReleaseNotes {
                version: "1.2.0".parse().unwrap(),
                notes: "## Changes\n\n- **Fixed** [a bug](https://example.com)\n- Added `Ctrl+J`\n\n3. First\n4. Second".into(),
            },
        ];
        assert_eq!(
            format_history(&history),
            "Version 1.2\r\n\r\nChanges\r\n\r\n• Fixed a bug\r\n• Added Ctrl+J\r\n\r\n3. First\r\n4. Second\r\n\r\nVersion 1.1\r\n\r\nNo release notes were provided."
        );
    }
}
