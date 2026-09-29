//! The commit panel: what part of the pull request the diff shows.

use crate::renderer::Theme;
use crate::text::measure::{text_width, truncate};
use prtui_core::CommitLog;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::sync::Arc;

const SHORT_OID: usize = 7;

/// Which part of the pull request the diff is showing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Pick {
    #[default]
    All,
    SinceReview,
    /// An inclusive run of commits, named by oid so a refetched log that
    /// gained or lost commits still finds them.
    Commits {
        first: Arc<str>,
        last: Arc<str>,
    },
}

/// What a row of the panel picks when activated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    All,
    SinceReview,
    /// By index into the log's commits.
    Commit(usize),
}

pub struct Rows {
    pub lines: Vec<Line<'static>>,
    pub entries: Vec<Option<Entry>>,
}

impl Rows {
    pub const fn len(&self) -> usize {
        self.lines.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn entry_at(&self, row: usize) -> Option<Entry> {
        self.entries.get(row).copied().flatten()
    }

    pub fn row_of(&self, entry: Entry) -> Option<usize> {
        self.entries.iter().position(|held| *held == Some(entry))
    }
}

pub fn short(oid: &str) -> &str {
    oid.get(..SHORT_OID).unwrap_or(oid)
}

/// The panel row the first commit is drawn on, below the picks that are not
/// commits and the heading over them.
pub const fn first_commit_row(log: &CommitLog) -> usize {
    if log.last_reviewed.is_some() { 4 } else { 3 }
}

/// How many commits landed after the one the viewer last reviewed, or `None`
/// when that commit is no longer on the branch.
pub fn unreviewed(log: &CommitLog) -> Option<usize> {
    let reviewed = log.last_reviewed.as_deref()?;
    let at = log
        .commits
        .iter()
        .position(|commit| &*commit.oid == reviewed)?;

    Some(log.commits.len() - at - 1)
}

/// The entry a pick is drawn as current on.
pub fn entry_of(log: &CommitLog, pick: &Pick) -> Option<Entry> {
    match pick {
        Pick::All => Some(Entry::All),
        Pick::SinceReview => {
            log.last_reviewed.is_some().then_some(Entry::SinceReview)
        }
        Pick::Commits { last, .. } => log
            .commits
            .iter()
            .position(|commit| commit.oid == *last)
            .map(Entry::Commit),
    }
}

fn picks(log: &CommitLog, pick: &Pick, index: usize) -> bool {
    let Pick::Commits { first, last } = pick else {
        return false;
    };
    let position =
        |oid: &Arc<str>| log.commits.iter().position(|c| c.oid == *oid);

    match (position(first), position(last)) {
        (Some(first), Some(last)) => (first..=last).contains(&index),
        _ => false,
    }
}

/// `span` is the inclusive run of commit indices a range selection covers.
pub fn build(
    log: &CommitLog,
    pick: &Pick,
    span: Option<(usize, usize)>,
    width: usize,
    theme: Theme,
) -> Rows {
    let mut rows = Rows {
        lines: Vec::new(),
        entries: Vec::new(),
    };
    let indent = |is_current: bool| {
        Span::styled(
            if is_current { "▎ " } else { "  " },
            Style::default().fg(theme.accent),
        )
    };
    let label = |is_current: bool| {
        if is_current {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.heading)
        }
    };

    rows.lines.push(Line::from(vec![
        indent(*pick == Pick::All),
        Span::styled("all changes", label(*pick == Pick::All)),
    ]));
    rows.entries.push(Some(Entry::All));

    if log.last_reviewed.is_some() {
        let detail = match unreviewed(log) {
            Some(0) => "  up to date".to_owned(),
            Some(1) => "  1 new commit".to_owned(),
            Some(count) => format!("  {count} new commits"),
            None => "  history rewritten since".to_owned(),
        };

        rows.lines.push(Line::from(vec![
            indent(*pick == Pick::SinceReview),
            Span::styled(
                "since your last review",
                label(*pick == Pick::SinceReview),
            ),
            Span::styled(detail, Style::default().fg(theme.dim)),
        ]));
        rows.entries.push(Some(Entry::SinceReview));
    }

    rows.lines.push(Line::default());
    rows.entries.push(None);
    rows.lines.push(Line::styled(
        format!("commits · {}", log.commits.len()),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    ));
    rows.entries.push(None);

    for (index, commit) in log.commits.iter().enumerate() {
        let date = commit.authored_at.get(..10).unwrap_or(&commit.authored_at);
        let byline = format!("  @{} · {date}", commit.author);
        let oid = format!("{} ", short(&commit.oid));
        let budget =
            width.saturating_sub(2 + text_width(&oid) + text_width(&byline));
        let title = truncate(&commit.title, budget);
        let pad = budget.saturating_sub(text_width(&title));

        let is_current = picks(log, pick, index);
        let mut line = Line::from(vec![
            indent(is_current),
            Span::styled(oid, Style::default().fg(theme.warning)),
            Span::styled(title, label(is_current)),
            Span::raw(" ".repeat(pad)),
            Span::styled(byline, Style::default().fg(theme.dim)),
        ]);
        if span.is_some_and(|(low, high)| (low..=high).contains(&index)) {
            line = line.style(Style::default().bg(theme.selection));
        }

        rows.lines.push(line);
        rows.entries.push(Some(Entry::Commit(index)));
    }

    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use prtui_core::Commit;

    fn log(oids: &[&str], last_reviewed: Option<&str>) -> CommitLog {
        CommitLog {
            commits: oids
                .iter()
                .map(|oid| Commit {
                    oid: (*oid).into(),
                    parent: None,
                    title: format!("commit {oid}"),
                    author: "tale".into(),
                    authored_at: "2026-09-24T10:00:00Z".into(),
                })
                .collect(),
            last_reviewed: last_reviewed.map(Into::into),
        }
    }

    #[test]
    fn counts_the_commits_after_the_last_review() {
        assert_eq!(unreviewed(&log(&["a", "b", "c"], Some("a"))), Some(2));
        assert_eq!(unreviewed(&log(&["a", "b", "c"], Some("c"))), Some(0));
        assert_eq!(unreviewed(&log(&["a", "b"], Some("gone"))), None);
        assert_eq!(unreviewed(&log(&["a"], None)), None);
    }

    #[test]
    fn offers_since_review_only_after_a_review() {
        let theme = Theme::dark();
        let fresh = build(&log(&["a"], None), &Pick::All, None, 80, theme);
        let reviewed =
            build(&log(&["a", "b"], Some("a")), &Pick::All, None, 80, theme);

        assert_eq!(fresh.row_of(Entry::SinceReview), None);
        assert_eq!(reviewed.row_of(Entry::SinceReview), Some(1));
        assert_eq!(
            reviewed.row_of(Entry::Commit(0)),
            Some(first_commit_row(&log(&["a"], Some("a"))))
        );
        assert_eq!(fresh.row_of(Entry::Commit(0)), Some(3));
        assert_eq!(reviewed.entry_at(3), None);
    }

    #[test]
    fn a_range_pick_is_current_on_its_last_commit() {
        let log = log(&["a", "b", "c"], None);
        let pick = Pick::Commits {
            first: "a".into(),
            last: "b".into(),
        };

        assert_eq!(entry_of(&log, &pick), Some(Entry::Commit(1)));
        assert!(picks(&log, &pick, 0));
        assert!(!picks(&log, &pick, 2));
    }
}
