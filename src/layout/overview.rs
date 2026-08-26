//! The pull request's own writing: its description, and the comments made
//! about the change rather than about a line of it.
//!
//! Prose wraps to a width and a width is a layout fact, so the lines are built
//! here and the view only paints them.

use crate::model::{Comment, PullRequest};
use crate::renderer::{Theme, markdown};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

pub fn build(
    pr: Option<&PullRequest>,
    discussion: &[Comment],
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }

    let Some(pr) = pr else {
        return vec![dim("still loading the pull request", theme)];
    };

    let mut lines = if pr.body.trim().is_empty() {
        vec![dim("no description", theme)]
    } else {
        markdown::render(&pr.body, width, theme)
    };

    for comment in discussion {
        lines.push(Line::default());
        lines.push(rule(width, theme));
        lines.push(Line::default());
        lines.push(header(comment, theme));
        lines.extend(markdown::render(&comment.body, width, theme));
    }

    lines
}

fn dim(text: &'static str, theme: Theme) -> Line<'static> {
    Line::styled(text, Style::default().fg(theme.dim))
}

/// The same token the submit form's divider and a markdown `---` are drawn in.
/// `hunk` reads as a rule in neither mode: it is a background color, and one
/// painted as text disappears into the terminal behind it.
fn rule(width: usize, theme: Theme) -> Line<'static> {
    Line::styled("─".repeat(width), Style::default().fg(theme.dim))
}

/// The same `@author · date` a thread card writes, so one comment reads the
/// same wherever it is met.
fn header(comment: &Comment, theme: Theme) -> Line<'static> {
    let date = comment.created_at.get(..10).unwrap_or(&comment.created_at);
    let mut text = format!("@{}", comment.author);

    if !date.is_empty() {
        text.push_str(" · ");
        text.push_str(date);
    }

    Line::from(Span::styled(
        text,
        Style::default()
            .fg(theme.heading)
            .add_modifier(Modifier::BOLD),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn pull_request(body: &str) -> PullRequest {
        PullRequest {
            id: Arc::from("PR_1"),
            number: 42,
            title: "Rework the row model".to_owned(),
            state: "OPEN".to_owned(),
            is_draft: false,
            author: "tale".to_owned(),
            base_ref: "main".to_owned(),
            head_ref: "rows".to_owned(),
            head_oid: Arc::from("abc123"),
            body: body.to_owned(),
        }
    }

    fn comment(author: &str, body: &str) -> Comment {
        Comment {
            id: Arc::from("IC_1"),
            rest_id: Some(7),
            author: author.to_owned(),
            body: body.to_owned(),
            created_at: "2024-04-29T14:06:54Z".to_owned(),
            is_pending: false,
        }
    }

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn an_empty_description_says_so_rather_than_showing_nothing() {
        let lines = build(Some(&pull_request("   ")), &[], 40, Theme::dark());

        assert_eq!(text(&lines), ["no description"]);
    }

    #[test]
    fn every_comment_is_headed_by_who_wrote_it_and_when() {
        let lines = build(
            Some(&pull_request("why")),
            &[comment("alice", "looks good"), comment("bob", "ship it")],
            40,
            Theme::dark(),
        );
        let text = text(&lines);

        assert_eq!(text.first().map(String::as_str), Some("why"));
        assert!(text.contains(&"@alice · 2024-04-29".to_owned()));
        assert!(text.contains(&"@bob · 2024-04-29".to_owned()));
        assert!(text.contains(&"looks good".to_owned()));
    }

    /// The separator used to be drawn in `hunk`, which is the color the status
    /// bar sits *behind*. As text it vanished into the terminal in both modes.
    #[test]
    fn the_separator_is_drawn_in_a_foreground_color() {
        for theme in [Theme::dark(), Theme::light()] {
            let lines = build(
                Some(&pull_request("why")),
                &[comment("alice", "looks good")],
                40,
                theme,
            );
            let separator = lines
                .iter()
                .find(|line| line.to_string().starts_with('─'))
                .expect("a comment is separated from what is above it");

            let color = separator.style.fg;
            assert_eq!(color, Some(theme.dim), "{:?}", theme.mode);
            assert_ne!(color, Some(theme.hunk), "{:?}", theme.mode);
        }
    }

    /// Nothing is drawn into a pane with no room in it.
    #[test]
    fn no_width_is_no_lines() {
        assert!(
            build(Some(&pull_request("why")), &[], 0, Theme::dark()).is_empty()
        );
    }
}
