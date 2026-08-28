//! The lines the summary panel shows about one pull request.
//!
//! Everything here is a count or a verdict: the panel answers whether a review
//! is worth opening, and the review surface answers everything after that.

use prtui::gh::{Checks, Reviews, Summary, Threads};
use prtui::renderer::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// Columns the labels are padded to, so the values line up under each other.
const LABEL_WIDTH: usize = 10;

/// Lines [`build`] always answers with, which is what the panel is sized to.
pub const LINE_COUNT: usize = 8;

pub fn build(summary: &Summary, theme: Theme) -> Vec<Line<'static>> {
    vec![
        Line::from(vec![
            Span::styled(
                format!("@{}", summary.author),
                Style::default()
                    .fg(theme.heading)
                    .add_modifier(Modifier::BOLD),
            ),
            dim("  ", theme),
            Span::styled(
                format!("{} ← {}", summary.base_ref, summary.head_ref),
                Style::default().fg(theme.muted),
            ),
        ]),
        Line::default(),
        row("checks", checks(&summary.checks, theme), theme),
        row("reviews", reviews(&summary.reviews, theme), theme),
        row("threads", threads(&summary.threads, theme), theme),
        row("comments", vec![count(summary.comments, theme.code)], theme),
        row("changes", changes(summary, theme), theme),
        row(
            "updated",
            vec![Span::styled(
                summary.updated_on.clone(),
                Style::default().fg(theme.muted),
            )],
            theme,
        ),
    ]
}

fn row(label: &str, values: Vec<Span<'static>>, theme: Theme) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{label:LABEL_WIDTH$}"),
        Style::default().fg(theme.dim),
    )];
    spans.extend(values);

    Line::from(spans)
}

fn checks(checks: &Checks, theme: Theme) -> Vec<Span<'static>> {
    if checks.total() == 0 {
        return vec![dim("no checks", theme)];
    }

    let mut parts = Vec::new();
    push(&mut parts, checks.passed, "passed", theme.success, theme);
    push(&mut parts, checks.failed, "failed", theme.danger, theme);
    push(&mut parts, checks.running, "running", theme.warning, theme);
    push(&mut parts, checks.skipped, "skipped", theme.muted, theme);

    parts
}

fn reviews(reviews: &Reviews, theme: Theme) -> Vec<Span<'static>> {
    let mut parts = Vec::new();
    push(
        &mut parts,
        reviews.approved,
        "approved",
        theme.success,
        theme,
    );
    push(
        &mut parts,
        reviews.changes_requested,
        "changes requested",
        theme.danger,
        theme,
    );
    push(
        &mut parts,
        reviews.commented,
        "commented",
        theme.muted,
        theme,
    );
    push(
        &mut parts,
        reviews.requested,
        "waiting",
        theme.warning,
        theme,
    );

    if parts.is_empty() {
        return vec![dim("nobody has looked yet", theme)];
    }

    parts
}

fn threads(threads: &Threads, theme: Theme) -> Vec<Span<'static>> {
    if threads.total == 0 {
        return vec![dim("no conversations", theme)];
    }

    let color = if threads.unresolved == 0 {
        theme.success
    } else {
        theme.warning
    };
    // A truncated page counted a floor rather than the whole tally.
    let unresolved = if threads.is_truncated {
        format!("{}+", threads.unresolved)
    } else {
        threads.unresolved.to_string()
    };

    vec![
        Span::styled(unresolved, Style::default().fg(color)),
        dim(" unresolved of ", theme),
        Span::styled(
            threads.total.to_string(),
            Style::default().fg(theme.muted),
        ),
    ]
}

fn changes(summary: &Summary, theme: Theme) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            format!("{} files", summary.changed_files),
            Style::default().fg(theme.code),
        ),
        dim(" · ", theme),
        Span::styled(
            format!("+{}", summary.additions),
            Style::default().fg(theme.success),
        ),
        dim(" ", theme),
        Span::styled(
            format!("−{}", summary.deletions),
            Style::default().fg(theme.danger),
        ),
    ]
}

fn push(
    parts: &mut Vec<Span<'static>>,
    tally: u32,
    label: &str,
    color: ratatui::style::Color,
    theme: Theme,
) {
    if tally == 0 {
        return;
    }

    if !parts.is_empty() {
        parts.push(dim(" · ", theme));
    }

    parts.push(Span::styled(
        format!("{tally} {label}"),
        Style::default().fg(color),
    ));
}

fn count(tally: u32, color: ratatui::style::Color) -> Span<'static> {
    Span::styled(tally.to_string(), Style::default().fg(color))
}

fn dim(text: &'static str, theme: Theme) -> Span<'static> {
    Span::styled(text, Style::default().fg(theme.dim))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary() -> Summary {
        Summary {
            author: "tale".into(),
            base_ref: "main".into(),
            head_ref: "rows".into(),
            additions: 120,
            deletions: 34,
            changed_files: 7,
            updated_on: "2026-08-20".into(),
            comments: 3,
            checks: Checks {
                passed: 12,
                failed: 1,
                running: 2,
                skipped: 0,
            },
            reviews: Reviews {
                approved: 2,
                changes_requested: 1,
                commented: 0,
                requested: 3,
            },
            threads: Threads {
                unresolved: 4,
                total: 11,
                is_truncated: false,
            },
        }
    }

    fn text(summary: &Summary) -> Vec<String> {
        build(summary, Theme::dark())
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn the_panel_is_sized_to_what_the_summary_writes() {
        assert_eq!(build(&summary(), Theme::dark()).len(), LINE_COUNT);
    }

    #[test]
    fn every_tally_is_counted_rather_than_listed() {
        let lines = text(&summary());

        assert!(lines.iter().any(|line| line.contains("@tale")));
        assert!(lines.iter().any(|line| line.contains("main ← rows")));
        assert!(lines.iter().any(|line| {
            line.contains("12 passed · 1 failed · 2 running")
        }));
        assert!(lines.iter().any(|line| {
            line.contains("2 approved · 1 changes requested · 3 waiting")
        }));
        assert!(lines.iter().any(|line| line.contains("4 unresolved of 11")));
        assert!(lines.iter().any(|line| line.contains("7 files · +120 −34")));
    }

    /// A count of zero says nothing, so a clean pull request reads as clean
    /// rather than as a row of zeroes.
    #[test]
    fn empty_tallies_are_left_out() {
        let mut summary = summary();
        summary.checks = Checks::default();
        summary.reviews = Reviews::default();
        summary.threads = Threads {
            unresolved: 0,
            total: 0,
            is_truncated: false,
        };
        let lines = text(&summary);

        assert!(lines.iter().any(|line| line.contains("no checks")));
        assert!(lines.iter().any(|line| line.contains("nobody has looked")));
        assert!(lines.iter().any(|line| line.contains("no conversations")));
    }

    /// More threads than the page counted makes the tally a floor.
    #[test]
    fn a_truncated_thread_page_says_so() {
        let mut summary = summary();
        summary.threads = Threads {
            unresolved: 100,
            total: 140,
            is_truncated: true,
        };

        assert!(
            text(&summary)
                .iter()
                .any(|line| line.contains("100+ unresolved of 140"))
        );
    }
}
