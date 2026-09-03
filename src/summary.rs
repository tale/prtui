//! The lines the summary panel shows about one pull request.
//!
//! It answers whether a review is worth opening: who it is waiting on, what is
//! failing, and how much is left unresolved. The reviewers are named, since
//! that is usually the reason a pull request is on the list at all. The checks
//! are folded away until asked for: a busy repository reports dozens, and the
//! tally is what a reader wants first.

use prtui::model::{Check, CheckState, Reviewer, Summary, Threads, Verdict};
use prtui::renderer::Theme;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Columns the labels are padded to, so the values line up under each other.
const LABEL_WIDTH: usize = 10;

/// Columns a named reviewer is padded to before their verdict.
const NAME_WIDTH: usize = 28;

/// The rows every summary writes, whatever the fold is doing: the head, the
/// blank under it, the two section headers with a blank between them, and the
/// four tallies at the foot.
const FIXED_LINES: usize = 10;

/// The row the checks fold sits on, which is the row `<CR>` opens it from.
pub const fn checks_row(summary: &Summary) -> usize {
    // The head, the blank under it, the reviewers and their blank.
    4 + summary.reviewers.len()
}

/// How tall [`build`] will be, which is what the panel sizes its cursor to.
pub const fn line_count(summary: &Summary, is_checks_open: bool) -> usize {
    let checks = if is_checks_open {
        summary.checks.len()
    } else {
        0
    };

    FIXED_LINES + summary.reviewers.len() + checks
}

pub fn build(
    summary: &Summary,
    is_checks_open: bool,
    theme: Theme,
) -> Vec<Line<'static>> {
    let mut lines = vec![
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
        row("  ", "reviewers", reviewer_tally(summary, theme), theme),
    ];

    lines.extend(
        summary
            .reviewers
            .iter()
            .map(|reviewer| reviewer_row(reviewer, theme)),
    );

    lines.push(Line::default());
    lines.push(row(
        if is_checks_open { "▾ " } else { "▸ " },
        "checks",
        check_tally(summary, theme),
        theme,
    ));

    if is_checks_open {
        lines
            .extend(summary.checks.iter().map(|check| check_row(check, theme)));
    }

    lines.extend([
        Line::default(),
        row("  ", "threads", threads(&summary.threads, theme), theme),
        row(
            "  ",
            "comments",
            vec![Span::styled(
                summary.comments.to_string(),
                Style::default().fg(theme.code),
            )],
            theme,
        ),
        row("  ", "changes", changes(summary, theme), theme),
        row(
            "  ",
            "updated",
            vec![Span::styled(
                summary.updated_on.clone(),
                Style::default().fg(theme.muted),
            )],
            theme,
        ),
    ]);

    lines
}

/// A labelled row, under the two-column gutter the fold marker sits in.
fn row(
    marker: &str,
    label: &str,
    values: Vec<Span<'static>>,
    theme: Theme,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            marker.to_owned(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{label:LABEL_WIDTH$}"),
            Style::default().fg(theme.dim),
        ),
    ];
    spans.extend(values);

    Line::from(spans)
}

fn reviewer_row(reviewer: &Reviewer, theme: Theme) -> Line<'static> {
    let color = verdict_color(reviewer.verdict, theme);
    let name = if reviewer.is_team {
        format!("@{} (team)", reviewer.name)
    } else {
        format!("@{}", reviewer.name)
    };

    Line::from(vec![
        Span::styled(
            format!("    {} ", verdict_glyph(reviewer.verdict)),
            Style::default().fg(color),
        ),
        Span::styled(
            format!("{name:NAME_WIDTH$}"),
            Style::default().fg(theme.code),
        ),
        Span::styled(
            reviewer.verdict.label().to_owned(),
            Style::default().fg(color),
        ),
    ])
}

fn check_row(check: &Check, theme: Theme) -> Line<'static> {
    let color = check_color(check.state, theme);

    Line::from(vec![
        Span::styled(
            format!("    {} ", check_glyph(check.state)),
            Style::default().fg(color),
        ),
        Span::styled(check.name.clone(), Style::default().fg(theme.code)),
    ])
}

fn reviewer_tally(summary: &Summary, theme: Theme) -> Vec<Span<'static>> {
    if summary.reviewers.is_empty() {
        return vec![dim("nobody has looked yet", theme)];
    }

    let mut parts = Vec::with_capacity(7);
    for verdict in [
        Verdict::ChangesRequested,
        Verdict::Waiting,
        Verdict::Commented,
        Verdict::Approved,
    ] {
        let tally = summary
            .reviewers
            .iter()
            .filter(|reviewer| reviewer.verdict == verdict)
            .count();

        push(
            &mut parts,
            tally,
            verdict.label(),
            verdict_color(verdict, theme),
            theme,
        );
    }

    parts
}

fn check_tally(summary: &Summary, theme: Theme) -> Vec<Span<'static>> {
    if summary.checks.is_empty() {
        return vec![dim("no checks", theme)];
    }

    let mut parts = Vec::with_capacity(7);
    for (state, label) in [
        (CheckState::Failed, "failed"),
        (CheckState::Running, "running"),
        (CheckState::Passed, "passed"),
        (CheckState::Skipped, "skipped"),
    ] {
        let tally = summary
            .checks
            .iter()
            .filter(|check| check.state == state)
            .count();

        push(&mut parts, tally, label, check_color(state, theme), theme);
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

const fn verdict_color(verdict: Verdict, theme: Theme) -> Color {
    match verdict {
        Verdict::Approved => theme.success,
        Verdict::ChangesRequested => theme.danger,
        Verdict::Waiting => theme.warning,
        Verdict::Commented => theme.muted,
    }
}

const fn verdict_glyph(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Approved => "✓",
        Verdict::ChangesRequested => "✗",
        Verdict::Waiting => "◦",
        Verdict::Commented => "·",
    }
}

const fn check_color(state: CheckState, theme: Theme) -> Color {
    match state {
        CheckState::Passed => theme.success,
        CheckState::Failed => theme.danger,
        CheckState::Running => theme.warning,
        CheckState::Skipped => theme.muted,
    }
}

const fn check_glyph(state: CheckState) -> &'static str {
    match state {
        CheckState::Passed => "✓",
        CheckState::Failed => "✗",
        CheckState::Running => "●",
        CheckState::Skipped => "⊘",
    }
}

fn push(
    parts: &mut Vec<Span<'static>>,
    tally: usize,
    label: &str,
    color: Color,
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

fn dim(text: &'static str, theme: Theme) -> Span<'static> {
    Span::styled(text, Style::default().fg(theme.dim))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, state: CheckState) -> Check {
        Check {
            name: name.into(),
            state,
        }
    }

    fn reviewer(name: &str, verdict: Verdict) -> Reviewer {
        Reviewer {
            name: name.into(),
            is_team: false,
            verdict,
        }
    }

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
            checks: vec![
                check("clippy", CheckState::Failed),
                check("deploy", CheckState::Running),
                check("build", CheckState::Passed),
            ],
            reviewers: vec![
                reviewer("bob", Verdict::ChangesRequested),
                Reviewer {
                    name: "owner/backend".into(),
                    is_team: true,
                    verdict: Verdict::Waiting,
                },
                reviewer("alice", Verdict::Approved),
            ],
            threads: Threads {
                unresolved: 4,
                total: 11,
                is_truncated: false,
            },
        }
    }

    fn text(summary: &Summary, is_checks_open: bool) -> Vec<String> {
        build(summary, is_checks_open, Theme::dark())
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn the_panel_is_sized_to_what_the_summary_writes() {
        let summary = summary();

        for is_checks_open in [false, true] {
            assert_eq!(
                build(&summary, is_checks_open, Theme::dark()).len(),
                line_count(&summary, is_checks_open)
            );
        }
    }

    /// Who a pull request is waiting on is the reason it is on the list, so
    /// the reviewers are named rather than counted.
    #[test]
    fn every_reviewer_is_named_with_where_they_stand() {
        let lines = text(&summary(), false);

        assert!(lines.iter().any(|line| {
            line.contains("1 changes requested · 1 waiting · 1 approved")
        }));
        assert!(lines.iter().any(|line| {
            line.contains("@bob") && line.contains("changes requested")
        }));
        assert!(
            lines.iter().any(
                |line| line.contains("@alice") && line.contains("approved")
            )
        );
    }

    /// A team is asked as a team: nobody on it has picked the review up yet.
    #[test]
    fn a_team_request_is_listed_as_the_team() {
        assert!(text(&summary(), false).iter().any(|line| {
            line.contains("@owner/backend (team)") && line.contains("waiting")
        }));
    }

    #[test]
    fn the_fold_row_is_where_the_checks_are_written() {
        let summary = summary();
        let lines = text(&summary, false);

        assert!(lines[checks_row(&summary)].contains("checks"));
    }

    #[test]
    fn the_checks_are_a_tally_until_the_fold_is_opened() {
        let folded = text(&summary(), false);

        assert!(folded.iter().any(|line| {
            line.contains('▸')
                && line.contains("1 failed · 1 running · 1 passed")
        }));
        assert!(!folded.iter().any(|line| line.contains("clippy")));

        let opened = text(&summary(), true);

        assert!(opened.iter().any(|line| line.contains('▾')));
        assert!(opened.iter().any(|line| line.contains("clippy")));
        assert!(opened.iter().any(|line| line.contains("deploy")));
    }

    /// A count of zero says nothing, so a clean pull request reads as clean
    /// rather than as a row of zeroes.
    #[test]
    fn empty_tallies_are_left_out() {
        let mut summary = summary();
        summary.checks = Vec::new();
        summary.reviewers = Vec::new();
        summary.threads = Threads {
            unresolved: 0,
            total: 0,
            is_truncated: false,
        };
        let lines = text(&summary, true);

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
            text(&summary, false)
                .iter()
                .any(|line| line.contains("100+ unresolved of 140"))
        );
    }
}
