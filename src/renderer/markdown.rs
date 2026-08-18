use super::Theme;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Default)]
struct StyledLine {
    spans: Vec<Span<'static>>,
    preserve_whitespace: bool,
}

/// A rendered comment is mostly text, but images are handed to the caller whole
/// so a terminal that can draw them is not stuck with their alt text.
pub enum Block {
    Text(Line<'static>),
    Image { url: String, alt: String },
}

enum Piece {
    Line(StyledLine),
    Image { url: String, alt: String },
}

impl Piece {
    const fn is_content(&self) -> bool {
        !matches!(self, Self::Line(line) if line.spans.is_empty())
    }
}

struct PendingImage {
    url: String,
    alt: String,
}

struct ListState {
    next: Option<u64>,
}

struct Builder {
    theme: Theme,
    lines: Vec<Piece>,
    current: StyledLine,
    style: Style,
    style_stack: Vec<Style>,
    lists: Vec<ListState>,
    quote_depth: usize,
    item_depth: usize,
    code_language: Option<String>,
    table_cell: usize,
    table_header: bool,
    in_summary: bool,
    image: Option<PendingImage>,
}

impl Builder {
    fn new(theme: Theme) -> Self {
        Self {
            theme,
            lines: Vec::new(),
            current: StyledLine::default(),
            style: Style::default().fg(theme.code),
            style_stack: Vec::new(),
            lists: Vec::new(),
            quote_depth: 0,
            item_depth: 0,
            code_language: None,
            table_cell: 0,
            table_header: false,
            in_summary: false,
            image: None,
        }
    }

    fn append(&mut self, text: impl Into<String>, style: Style) {
        let text = text.into();
        if text.is_empty() {
            return;
        }
        self.ensure_quote_prefix();
        push_span(&mut self.current.spans, text, style);
    }

    fn append_current(&mut self, text: impl Into<String>) {
        if let Some(image) = &mut self.image {
            image.alt.push_str(&text.into());
            return;
        }

        let style = if self.in_summary {
            self.style
                .fg(self.theme.heading)
                .add_modifier(Modifier::BOLD)
        } else if self.table_header {
            self.style.add_modifier(Modifier::BOLD)
        } else {
            self.style
        };
        self.append(text, style);
    }

    fn ensure_quote_prefix(&mut self) {
        if !self.current.spans.is_empty() || self.quote_depth == 0 {
            return;
        }
        push_span(
            &mut self.current.spans,
            "│ ".repeat(self.quote_depth),
            Style::default().fg(self.theme.purple),
        );
    }

    fn finish_line(&mut self) {
        if self.current.spans.is_empty() {
            return;
        }
        self.lines
            .push(Piece::Line(std::mem::take(&mut self.current)));
    }

    fn blank_line(&mut self) {
        self.finish_line();
        if self.lines.last().is_some_and(Piece::is_content) {
            self.lines.push(Piece::Line(StyledLine::default()));
        }
    }

    fn push_image(&mut self, url: String, alt: String) {
        self.finish_line();
        self.lines.push(Piece::Image { url, alt });
    }

    fn push_style(&mut self, style: Style) {
        self.style_stack.push(self.style);
        self.style = style;
    }

    fn pop_style(&mut self) {
        if let Some(style) = self.style_stack.pop() {
            self.style = style;
        }
    }

    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                if self.item_depth == 0 {
                    self.finish_line();
                }
            }
            Tag::Heading { .. } => {
                self.finish_line();
                self.append(
                    "▸ ",
                    Style::default()
                        .fg(self.theme.accent)
                        .add_modifier(Modifier::BOLD),
                );
                self.push_style(
                    self.style
                        .fg(self.theme.heading)
                        .add_modifier(Modifier::BOLD),
                );
            }
            Tag::BlockQuote(_) => {
                self.finish_line();
                self.quote_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                self.finish_line();
                let language = match kind {
                    CodeBlockKind::Indented => String::new(),
                    CodeBlockKind::Fenced(language) => language.into_string(),
                };
                let heading = if language.is_empty() {
                    "┌─ code".into()
                } else {
                    format!("┌─ {language}")
                };
                self.append(heading, Style::default().fg(self.theme.dim));
                self.finish_line();
                self.code_language = Some(language);
            }
            Tag::List(start) => {
                self.finish_line();
                self.lists.push(ListState { next: start });
            }
            Tag::Item => {
                self.finish_line();
                self.item_depth += 1;
                let indent = "  ".repeat(self.lists.len().saturating_sub(1));
                let marker = match self
                    .lists
                    .last_mut()
                    .and_then(|list| list.next.as_mut())
                {
                    Some(next) => {
                        let marker = format!("{next}. ");
                        *next += 1;
                        marker
                    }
                    None => "• ".into(),
                };
                self.append(
                    format!("{indent}{marker}"),
                    Style::default().fg(self.theme.purple),
                );
            }
            Tag::FootnoteDefinition(label) => {
                self.finish_line();
                self.append(
                    format!("[{}] ", label.into_string()),
                    Style::default().fg(self.theme.muted),
                );
            }
            Tag::Table(_) | Tag::TableRow => {
                self.finish_line();
                self.table_cell = 0;
            }
            Tag::TableHead => {
                self.finish_line();
                self.table_cell = 0;
                self.table_header = true;
            }
            Tag::TableCell => {
                if self.table_cell > 0 {
                    self.append(" │ ", Style::default().fg(self.theme.dim));
                }
                self.table_cell += 1;
            }
            Tag::Emphasis => {
                self.push_style(self.style.add_modifier(Modifier::ITALIC));
            }
            Tag::Strong => {
                self.push_style(self.style.add_modifier(Modifier::BOLD));
            }
            Tag::Strikethrough => {
                self.push_style(self.style.add_modifier(Modifier::CROSSED_OUT));
            }
            Tag::Link { .. } => {
                self.push_style(
                    self.style
                        .fg(self.theme.accent)
                        .add_modifier(Modifier::UNDERLINED),
                );
            }
            Tag::Image { dest_url, .. } => {
                self.finish_line();
                self.image = Some(PendingImage {
                    url: dest_url.into_string(),
                    alt: String::new(),
                });
            }
            Tag::DefinitionList => self.finish_line(),
            Tag::DefinitionListTitle => {
                self.finish_line();
                self.push_style(self.style.add_modifier(Modifier::BOLD));
            }
            Tag::DefinitionListDefinition => {
                self.finish_line();
                self.append("  ", self.style);
            }
            Tag::Superscript | Tag::Subscript => {
                self.push_style(self.style.fg(self.theme.muted));
            }
            Tag::HtmlBlock | Tag::MetadataBlock(_) => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.finish_line();
                if self.item_depth == 0 {
                    self.blank_line();
                }
            }
            TagEnd::Heading(_) => {
                self.pop_style();
                self.finish_line();
                self.blank_line();
            }
            TagEnd::BlockQuote(_) => {
                self.finish_line();
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.blank_line();
            }
            TagEnd::CodeBlock => {
                self.finish_line();
                self.append("└─", Style::default().fg(self.theme.dim));
                self.finish_line();
                self.blank_line();
                self.code_language = None;
            }
            TagEnd::List(_) => {
                self.finish_line();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.blank_line();
                }
            }
            TagEnd::Item => {
                self.finish_line();
                self.item_depth = self.item_depth.saturating_sub(1);
            }
            TagEnd::FootnoteDefinition => {
                self.finish_line();
                self.blank_line();
            }
            TagEnd::TableHead => {
                self.finish_line();
                self.table_header = false;
            }
            TagEnd::Table => self.blank_line(),
            TagEnd::Image => {
                if let Some(image) = self.image.take() {
                    self.push_image(image.url, image.alt);
                }
            }
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Link
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::DefinitionListTitle => self.pop_style(),
            TagEnd::DefinitionListDefinition | TagEnd::TableRow => {
                self.finish_line();
            }
            TagEnd::DefinitionList
            | TagEnd::TableCell
            | TagEnd::HtmlBlock
            | TagEnd::MetadataBlock(_) => {}
        }
    }

    fn code_text(&mut self, text: &str) {
        let is_diff = self.code_language.as_deref() == Some("diff");
        let normalized = text.replace('\r', "");
        let lines: Vec<&str> = normalized.split('\n').collect();
        // The parser commonly includes one terminal newline in code events.
        let count = lines
            .len()
            .saturating_sub(usize::from(lines.last() == Some(&"")));
        for line in lines.into_iter().take(count) {
            self.append("│ ", Style::default().fg(self.theme.dim));
            let color = if is_diff {
                match line.chars().next() {
                    Some('+') => self.theme.success,
                    Some('-') => self.theme.danger,
                    Some('@') => self.theme.muted,
                    _ => self.theme.code,
                }
            } else {
                self.theme.code
            };
            self.append(line, Style::default().fg(color));
            self.current.preserve_whitespace = true;
            self.finish_line();
        }
    }

    fn html(&mut self, raw: &str) {
        let lower = raw.to_ascii_lowercase();
        if lower.contains("<br") {
            self.finish_line();
        }

        // Attachments dropped into a comment often arrive as raw <img> tags.
        if lower.contains("<img") {
            for (url, alt) in img_tags(raw) {
                self.push_image(url, alt);
            }
            return;
        }

        if let Some(summary_start) = lower.find("<summary") {
            let Some(content_start) = raw[summary_start..].find('>') else {
                self.in_summary = true;
                self.append(
                    "▾ ",
                    Style::default()
                        .fg(self.theme.accent)
                        .add_modifier(Modifier::BOLD),
                );
                return;
            };
            let content_start = summary_start + content_start + 1;
            if let Some(content_end) = lower[content_start..].find("</summary>")
            {
                let text = strip_html(
                    &raw[content_start..content_start + content_end],
                );
                self.append(
                    format!("▾ {text}"),
                    Style::default()
                        .fg(self.theme.heading)
                        .add_modifier(Modifier::BOLD),
                );
                self.finish_line();
                return;
            }
            self.in_summary = true;
            self.append(
                "▾ ",
                Style::default()
                    .fg(self.theme.accent)
                    .add_modifier(Modifier::BOLD),
            );
            return;
        }

        if lower.contains("</summary") {
            self.in_summary = false;
            self.finish_line();
            return;
        }
        if lower.trim().starts_with("<details")
            || lower.trim().starts_with("</details")
        {
            return;
        }

        let text = strip_html(raw);
        for (index, line) in text.lines().enumerate() {
            if index > 0 {
                self.finish_line();
            }
            self.append_current(line.to_string());
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) if self.code_language.is_some() => {
                self.code_text(&text);
            }
            Event::Text(text) => {
                match image_link(&text).filter(|_| self.image.is_none()) {
                    Some(url) => self.push_image(url, String::new()),
                    None => self.append_current(text.into_string()),
                }
            }
            Event::Code(code) => self.append(
                code.into_string(),
                self.style.fg(self.theme.orange).bg(self.theme.cursor),
            ),
            Event::InlineMath(math) | Event::DisplayMath(math) => {
                self.append(
                    math.into_string(),
                    self.style.fg(self.theme.orange),
                );
            }
            Event::Html(html) | Event::InlineHtml(html) => self.html(&html),
            Event::FootnoteReference(label) => self.append(
                format!("[{}]", label.into_string()),
                self.style.fg(self.theme.accent),
            ),
            Event::SoftBreak => self.append_current(" "),
            Event::HardBreak => self.finish_line(),
            Event::Rule => {
                self.finish_line();
                self.append(
                    "─".repeat(12),
                    Style::default().fg(self.theme.dim),
                );
                self.finish_line();
            }
            Event::TaskListMarker(checked) => self.append(
                if checked { "☑ " } else { "☐ " },
                Style::default().fg(if checked {
                    self.theme.success
                } else {
                    self.theme.muted
                }),
            ),
        }
    }

    fn finish(mut self) -> Vec<Piece> {
        self.finish_line();
        while self.lines.last().is_some_and(|piece| !piece.is_content()) {
            self.lines.pop();
        }
        self.lines
    }
}

/// Text lines plus the images the comment referenced, in document order.
pub fn render_blocks(body: &str, width: usize, theme: Theme) -> Vec<Block> {
    if width == 0 {
        return Vec::new();
    }

    build(body, theme)
        .into_iter()
        .flat_map(|piece| match piece {
            Piece::Line(line) => wrap_line(line, width)
                .into_iter()
                .map(Block::Text)
                .collect(),
            Piece::Image { url, alt } => vec![Block::Image { url, alt }],
        })
        .collect()
}

/// Rendered text only; images degrade to a single labelled line.
pub fn render(body: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    render_blocks(body, width, theme)
        .into_iter()
        .flat_map(|block| match block {
            Block::Text(line) => vec![line],
            Block::Image { url, alt } => {
                image_lines(&url, &alt, None, width, theme)
            }
        })
        .collect()
}

fn build(body: &str, theme: Theme) -> Vec<Piece> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM;
    let mut builder = Builder::new(theme);
    for event in Parser::new_ext(body, options) {
        builder.event(event);
    }
    builder.finish()
}

/// An image the caller will not draw, shown as its link. `reason` leads because
/// an attachment URL is a UUID that wraps away, and why it is missing should not.
pub fn image_lines(
    url: &str,
    alt: &str,
    reason: Option<&str>,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let label = if alt.trim().is_empty() { url } else { alt };
    let mut spans = vec![Span::styled("▭ ", Style::default().fg(theme.muted))];

    if let Some(reason) = reason {
        spans.push(Span::styled(
            format!("{reason} · "),
            Style::default().fg(theme.muted),
        ));
    }
    spans.push(Span::styled(
        label.to_string(),
        Style::default().fg(theme.accent),
    ));

    wrap_line(
        StyledLine {
            spans,
            preserve_whitespace: false,
        },
        width,
    )
}

/// GitHub renders an attachment URL pasted on its own as the image itself, and
/// that is how the web UI writes uploads into a comment body.
fn image_link(text: &str) -> Option<String> {
    const ATTACHMENTS: [&str; 3] = [
        "https://github.com/user-attachments/assets/",
        "https://user-images.githubusercontent.com/",
        "https://private-user-images.githubusercontent.com/",
    ];
    const EXTENSIONS: [&str; 5] = [".png", ".jpg", ".jpeg", ".gif", ".webp"];

    let url = text.trim();
    if url.is_empty()
        || url.contains(char::is_whitespace)
        || !url.starts_with("https://")
    {
        return None;
    }

    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    let is_image = ATTACHMENTS.iter().any(|prefix| url.starts_with(prefix))
        || EXTENSIONS.iter().any(|extension| path.ends_with(extension));

    is_image.then(|| url.to_string())
}

/// Source URL and alt text of every `<img>` in a raw HTML chunk.
fn img_tags(raw: &str) -> Vec<(String, String)> {
    let lower = raw.to_ascii_lowercase();
    let mut tags = Vec::new();
    let mut cursor = 0;

    while let Some(offset) = lower[cursor..].find("<img") {
        let start = cursor + offset;
        let end = lower[start..]
            .find('>')
            .map_or(raw.len(), |offset| start + offset);
        cursor = end.max(start + 4);

        let Some(url) = attribute(&raw[start..end], "src") else {
            continue;
        };
        tags.push((
            url,
            attribute(&raw[start..end], "alt").unwrap_or_default(),
        ));
    }

    tags
}

/// Lowercasing is byte-length preserving for ASCII, so offsets found in the
/// lowered copy index the original safely.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let start =
        tag.to_ascii_lowercase().find(&format!("{name}="))? + name.len() + 1;
    let value = tag.get(start..)?;

    let quote = value.chars().next()?;
    if quote != '"' && quote != '\'' {
        return value.split_whitespace().next().map(str::to_string);
    }

    let value = &value[quote.len_utf8()..];
    Some(value[..value.find(quote)?].to_string())
}

fn wrap_line(line: StyledLine, width: usize) -> Vec<Line<'static>> {
    if line.spans.is_empty() {
        return vec![Line::default()];
    }
    if line.preserve_whitespace {
        return wrap_preserving_whitespace(line.spans, width);
    }

    let mut rows = Vec::new();
    let mut current = Vec::new();
    let mut used = 0;
    let mut pending_space: Option<Style> = None;

    for span in line.spans {
        for (token, whitespace) in text_tokens(&span.content) {
            if whitespace {
                if used > 0 {
                    pending_space = Some(span.style);
                }
                continue;
            }

            let token_width = UnicodeWidthStr::width(token.as_str());
            let space_width = usize::from(pending_space.is_some() && used > 0);
            if used > 0 && used + space_width + token_width > width {
                rows.push(Line::from(std::mem::take(&mut current)));
                used = 0;
                pending_space = None;
            }

            if let Some(space_style) = pending_space.take().filter(|_| used > 0)
            {
                push_span(&mut current, " ", space_style);
                used += 1;
            }
            push_hard_wrapped(
                &mut rows,
                &mut current,
                &mut used,
                &token,
                span.style,
                width,
            );
        }
    }

    if !current.is_empty() {
        rows.push(Line::from(current));
    }
    rows
}

fn wrap_preserving_whitespace(
    spans: Vec<Span<'static>>,
    width: usize,
) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    let mut current = Vec::new();
    let mut used = 0;
    for span in spans {
        for character in span.content.chars() {
            let character_width =
                UnicodeWidthChar::width(character).unwrap_or(0);
            if used > 0 && used + character_width > width {
                rows.push(Line::from(std::mem::take(&mut current)));
                used = 0;
            }
            push_span(&mut current, character.to_string(), span.style);
            used += character_width;
        }
    }
    if !current.is_empty() {
        rows.push(Line::from(current));
    }
    rows
}

fn push_hard_wrapped(
    rows: &mut Vec<Line<'static>>,
    current: &mut Vec<Span<'static>>,
    used: &mut usize,
    text: &str,
    style: Style,
    width: usize,
) {
    for character in text.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if *used > 0 && *used + character_width > width {
            rows.push(Line::from(std::mem::take(current)));
            *used = 0;
        }
        push_span(current, character.to_string(), style);
        *used += character_width;
    }
}

fn push_span(
    spans: &mut Vec<Span<'static>>,
    text: impl Into<String>,
    style: Style,
) {
    let text = text.into();
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut().filter(|span| span.style == style) {
        last.content.to_mut().push_str(&text);
    } else {
        spans.push(Span::styled(text, style));
    }
}

fn text_tokens(text: &str) -> Vec<(String, bool)> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut whitespace = None;
    for character in text.chars() {
        let is_whitespace = character.is_whitespace();
        if whitespace.is_some_and(|value| value != is_whitespace) {
            tokens.push((std::mem::take(&mut current), whitespace.unwrap()));
        }
        whitespace = Some(is_whitespace);
        current.push(character);
    }
    if let Some(whitespace) = whitespace {
        tokens.push((current, whitespace));
    }
    tokens
}

fn strip_html(raw: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for character in raw.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(character),
            _ => {}
        }
    }
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered_text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn hides_markdown_delimiters_and_preserves_semantics() {
        let lines = render(
            "_Minor_ and **important** with `code` and ~~obsolete~~",
            80,
            Theme::dark(),
        );
        assert_eq!(
            rendered_text(&lines),
            "Minor and important with code and obsolete"
        );

        let spans: Vec<&Span<'_>> =
            lines.iter().flat_map(|line| &line.spans).collect();
        assert!(spans.iter().any(|span| {
            span.content.contains("Minor")
                && span.style.add_modifier.contains(Modifier::ITALIC)
        }));
        assert!(spans.iter().any(|span| {
            span.content.contains("important")
                && span.style.add_modifier.contains(Modifier::BOLD)
        }));
        assert!(spans.iter().any(|span| {
            span.content.contains("obsolete")
                && span.style.add_modifier.contains(Modifier::CROSSED_OUT)
        }));
    }

    #[test]
    fn renders_gfm_details_tasks_and_diff_blocks() {
        let markdown = r"<details>
<summary>Proposed fix</summary>

- [x] handled

```diff
-old
+new
```
</details>";
        let lines = render(markdown, 80, Theme::dark());
        let text = rendered_text(&lines);

        assert!(text.contains("▾ Proposed fix"));
        assert!(text.contains("• ☑ handled"));
        assert!(text.contains("┌─ diff"));
        assert!(text.contains("│ -old"));
        assert!(text.contains("│ +new"));
    }

    #[test]
    fn lifts_markdown_and_html_images_out_of_the_text() {
        let markdown = "Before\n\n![the fix](https://example.com/a.png)\n\nAfter\n\n\
             <img width=\"600\" alt=\"raw tag\" src='https://example.com/b.png' />";
        let blocks = render_blocks(markdown, 40, Theme::dark());

        let images: Vec<(&str, &str)> = blocks
            .iter()
            .filter_map(|block| match block {
                Block::Image { url, alt } => Some((url.as_str(), alt.as_str())),
                Block::Text(_) => None,
            })
            .collect();
        assert_eq!(
            images,
            vec![
                ("https://example.com/a.png", "the fix"),
                ("https://example.com/b.png", "raw tag"),
            ]
        );
        // Callers that cannot draw images still see the surrounding prose.
        let text = rendered_text(&render(markdown, 40, Theme::dark()));
        assert!(text.contains("Before"));
        assert!(text.contains("▭ the fix"));
        assert!(text.contains("After"));
    }

    #[test]
    fn treats_a_bare_attachment_url_as_the_image_it_renders_to() {
        let urls = |body: &str| -> Vec<String> {
            render_blocks(body, 40, Theme::dark())
                .into_iter()
                .filter_map(|block| match block {
                    Block::Image { url, .. } => Some(url),
                    Block::Text(_) => None,
                })
                .collect()
        };

        let body = "this pattern is really weird to me.\n\n\
             https://github.com/user-attachments/assets/a9f8c825-a13f-4760-ae7b-6402471435aa";
        assert_eq!(
            urls(body),
            vec![
                "https://github.com/user-attachments/assets/a9f8c825-a13f-4760-ae7b-6402471435aa"
                    .to_string()
            ]
        );

        assert_eq!(
            urls("look at https://example.com/shot.png inline"),
            Vec::<String>::new(),
            "a URL mixed into a sentence stays prose"
        );
        assert_eq!(
            urls("https://example.com/pull/9000"),
            Vec::<String>::new(),
            "an ordinary link is not an image"
        );
        assert_eq!(
            urls("https://example.com/a.PNG?raw=1"),
            vec!["https://example.com/a.PNG?raw=1".to_string()]
        );
    }
}
