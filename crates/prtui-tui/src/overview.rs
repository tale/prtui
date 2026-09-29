//! Shared pull request overview rows and folds.

use crate::renderer::{Theme, markdown};
use crate::summary;
use prtui_core::{Comment, Review, Summary};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fold {
    Checks,
    Comment(Arc<str>),
}

#[derive(Default)]
pub struct FoldState {
    is_checks_open: bool,
    open_comments: HashSet<Arc<str>>,
}

impl FoldState {
    pub fn toggle(&mut self, fold: &Fold) {
        match fold {
            Fold::Checks => self.is_checks_open = !self.is_checks_open,
            Fold::Comment(id) if self.open_comments.remove(id) => {}
            Fold::Comment(id) => {
                self.open_comments.insert(Arc::clone(id));
            }
        }
    }

    fn is_comment_open(&self, id: &Arc<str>) -> bool {
        self.open_comments.contains(id)
    }
}

pub struct Rows {
    pub lines: Vec<Line<'static>>,
    pub(crate) folds: Vec<Option<Fold>>,
}

impl Rows {
    pub const fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn fold_at(&self, row: usize) -> Option<&Fold> {
        self.folds.get(row)?.as_ref()
    }
}

pub fn build(
    summary: &Summary,
    body: &str,
    discussion: &[Comment],
    reviews: &[Review],
    state: &FoldState,
    width: usize,
    theme: Theme,
) -> Rows {
    let mut lines = summary::build(summary, state.is_checks_open, theme);
    let mut folds = vec![None; lines.len()];
    folds[summary::checks_row(summary)] = Some(Fold::Checks);

    push_section(&mut lines, &mut folds, "description", theme);
    if body.trim().is_empty() {
        push(&mut lines, &mut folds, dim("no description", theme), None);
    } else {
        for line in markdown::render(body, width, theme) {
            push(&mut lines, &mut folds, line, None);
        }
    }

    push_section(&mut lines, &mut folds, "discussion", theme);
    if discussion.is_empty() && reviews.is_empty() {
        push(&mut lines, &mut folds, dim("no comments", theme), None);
    }

    for entry in timeline(discussion, reviews) {
        let (id, body) = match entry {
            Entry::Comment(comment) => (&comment.id, &comment.body),
            Entry::Review(review) => (&review.id, &review.body),
        };
        let is_foldable =
            matches!(entry, Entry::Comment(_)) || !body.trim().is_empty();
        let is_open = is_foldable && state.is_comment_open(id);
        let header = match entry {
            Entry::Comment(comment) => comment_header(comment, is_open, theme),
            Entry::Review(review) => review_header(review, is_open, theme),
        };
        let fold = is_foldable.then(|| Fold::Comment(Arc::clone(id)));
        push(&mut lines, &mut folds, header, fold);

        if !is_open {
            continue;
        }

        for line in markdown::render(body, width.saturating_sub(4), theme) {
            push(&mut lines, &mut folds, indent(line), None);
        }
    }

    Rows { lines, folds }
}

enum Entry<'a> {
    Comment(&'a Comment),
    Review(&'a Review),
}

/// Oldest first; an undated comment is a draft, which has not happened yet.
fn timeline<'a>(
    discussion: &'a [Comment],
    reviews: &'a [Review],
) -> Vec<Entry<'a>> {
    let mut entries: Vec<Entry<'a>> = discussion
        .iter()
        .map(Entry::Comment)
        .chain(reviews.iter().map(Entry::Review))
        .collect();

    entries.sort_by_key(|entry| {
        let at = match entry {
            Entry::Comment(comment) => comment.created_at.as_str(),
            Entry::Review(review) => review.submitted_at.as_str(),
        };
        (at.is_empty(), at)
    });
    entries
}

fn push_section(
    lines: &mut Vec<Line<'static>>,
    folds: &mut Vec<Option<Fold>>,
    title: &'static str,
    theme: Theme,
) {
    push(lines, folds, Line::default(), None);
    push(
        lines,
        folds,
        Line::styled(
            title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        None,
    );
}

fn push(
    lines: &mut Vec<Line<'static>>,
    folds: &mut Vec<Option<Fold>>,
    line: Line<'static>,
    fold: Option<Fold>,
) {
    lines.push(line);
    folds.push(fold);
}

fn comment_header(
    comment: &Comment,
    is_open: bool,
    theme: Theme,
) -> Line<'static> {
    let marker = if is_open { "▾ " } else { "▸ " };
    let date = comment.created_at.get(..10).unwrap_or(&comment.created_at);
    let mut spans = vec![
        Span::styled(
            marker,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("@{}", comment.author),
            Style::default()
                .fg(theme.heading)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    if !date.is_empty() {
        spans.push(Span::styled(
            format!(" · {date}"),
            Style::default().fg(theme.dim),
        ));
    }

    Line::from(spans)
}

fn review_header(
    review: &Review,
    is_open: bool,
    theme: Theme,
) -> Line<'static> {
    let marker = match (review.body.trim().is_empty(), is_open) {
        (true, _) => "  ",
        (false, true) => "▾ ",
        (false, false) => "▸ ",
    };
    let date = review
        .submitted_at
        .get(..10)
        .unwrap_or(&review.submitted_at);
    let color = summary::verdict_color(review.verdict, theme);
    let mut spans = vec![
        Span::styled(
            marker,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("@{}", review.author),
            Style::default()
                .fg(theme.heading)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            format!(
                "{} {}",
                summary::verdict_glyph(review.verdict),
                review.verdict.label()
            ),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ];

    if !date.is_empty() {
        spans.push(Span::styled(
            format!(" · {date}"),
            Style::default().fg(theme.dim),
        ));
    }
    if review.comments > 0 {
        let noun = if review.comments == 1 {
            "comment"
        } else {
            "comments"
        };
        spans.push(Span::styled(
            format!(" · {} {noun}", review.comments),
            Style::default().fg(theme.dim),
        ));
    }

    Line::from(spans)
}

fn indent(mut line: Line<'static>) -> Line<'static> {
    line.spans.insert(0, Span::raw("    "));
    line
}

fn dim(text: &'static str, theme: Theme) -> Line<'static> {
    Line::styled(text, Style::default().fg(theme.dim))
}

#[cfg(test)]
mod tests {
    use super::*;
    use prtui_core::{Check, CheckState, Review, Reviewer, Threads, Verdict};

    fn summary() -> Summary {
        Summary {
            author: "tale".into(),
            base_ref: "main".into(),
            head_ref: "work".into(),
            additions: 4,
            deletions: 2,
            changed_files: 1,
            updated_on: "2026-09-03".into(),
            comments: 1,
            checks: vec![Check {
                name: "build".into(),
                state: CheckState::Passed,
            }],
            reviewers: vec![Reviewer {
                name: "alice".into(),
                is_team: false,
                verdict: Verdict::Approved,
            }],
            threads: Threads {
                unresolved: 0,
                total: 0,
                is_truncated: false,
            },
        }
    }

    fn comment() -> Comment {
        Comment {
            id: "IC_1".into(),
            reply_target: None,
            author: "alice".into(),
            body: "ship it".into(),
            created_at: "2026-09-03T10:00:00Z".into(),
            is_pending: false,
        }
    }

    #[test]
    fn comments_are_individual_folds_closed_by_default() {
        let rows = build(
            &summary(),
            "why",
            &[comment()],
            &[],
            &FoldState::default(),
            40,
            Theme::dark(),
        );
        let header = rows
            .lines
            .iter()
            .position(|line| line.to_string().contains("@alice ·"))
            .unwrap();

        assert!(matches!(rows.fold_at(header), Some(Fold::Comment(_))));
        assert!(
            !rows
                .lines
                .iter()
                .any(|line| line.to_string() == "    ship it")
        );
    }

    #[test]
    fn only_the_selected_comment_opens() {
        let comment = comment();
        let mut state = FoldState::default();
        state.toggle(&Fold::Comment(Arc::clone(&comment.id)));
        let rows = build(
            &summary(),
            "why",
            &[comment],
            &[],
            &state,
            40,
            Theme::dark(),
        );

        assert!(
            rows.lines
                .iter()
                .any(|line| line.to_string() == "    ship it")
        );
    }

    fn review(id: &str, body: &str, submitted_at: &str) -> Review {
        Review {
            id: id.into(),
            author: "bob".into(),
            verdict: Verdict::ChangesRequested,
            body: body.into(),
            submitted_at: submitted_at.into(),
            comments: 3,
        }
    }

    fn row_of(rows: &Rows, needle: &str) -> usize {
        rows.lines
            .iter()
            .position(|line| line.to_string().contains(needle))
            .unwrap()
    }

    #[test]
    fn reviews_interleave_with_comments_by_time() {
        let reviews = [
            review("PRR_1", "", "2026-09-02T10:00:00Z"),
            review("PRR_2", "", "2026-09-04T10:00:00Z"),
        ];
        let rows = build(
            &summary(),
            "why",
            &[comment()],
            &reviews,
            &FoldState::default(),
            60,
            Theme::dark(),
        );

        let first = row_of(&rows, "2026-09-02");
        let middle = row_of(&rows, "@alice ·");
        let last = row_of(&rows, "2026-09-04");

        assert!(first < middle && middle < last);
        assert!(rows.lines[first].to_string().contains("changes requested"));
        assert!(rows.lines[first].to_string().contains("3 comments"));
    }

    #[test]
    fn only_a_review_with_a_summary_folds() {
        let reviews = [
            review("PRR_1", "", "2026-09-02T10:00:00Z"),
            review("PRR_2", "fix the tests", "2026-09-04T10:00:00Z"),
        ];
        let mut state = FoldState::default();
        state.toggle(&Fold::Comment("PRR_2".into()));
        let rows =
            build(&summary(), "why", &[], &reviews, &state, 60, Theme::dark());

        assert!(rows.fold_at(row_of(&rows, "2026-09-02")).is_none());
        assert!(rows.fold_at(row_of(&rows, "2026-09-04")).is_some());
        assert!(
            rows.lines
                .iter()
                .any(|line| line.to_string() == "    fix the tests")
        );
    }
}
