//! Where the pull request lives on the web, and what the loop does with it.

use crate::text::url::escape_path;

/// Something the loop does outside the model: hand a link to the browser, or
/// put one on the clipboard. Both leave the process, so neither is `apply`'s to
/// carry out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Errand {
    Open(String),
    Copy(String),
}

/// The web address of the pull request under review.
///
/// Session configuration the app is handed once, the same way the theme is.
/// Without it nothing can be linked, which is what the `Option` on the app
/// means rather than a failure to report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// `https://host/owner/repo`, with no trailing slash.
    pub repo_url: String,
    pub number: u32,
}

impl Origin {
    pub fn pull_url(&self) -> String {
        format!("{}/pull/{}", self.repo_url, self.number)
    }

    /// A review comment is addressed by its REST id even on the web, which is
    /// why a thread with no id behind it cannot be linked to.
    pub fn comment_url(&self, rest_id: u64) -> String {
        format!("{}#discussion_r{rest_id}", self.pull_url())
    }

    /// A file at one commit, which is what makes the link a permalink: a
    /// branch name moves, a commit does not.
    pub fn blob_url(
        &self,
        commit: &str,
        path: &str,
        lines: Option<(u32, u32)>,
    ) -> String {
        let base =
            format!("{}/blob/{commit}/{}", self.repo_url, escape_path(path));

        match lines {
            None => base,
            Some((start, end)) if start == end => format!("{base}#L{start}"),
            Some((start, end)) => format!("{base}#L{start}-L{end}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> Origin {
        Origin {
            repo_url: "https://github.com/tale/prtui".to_owned(),
            number: 42,
        }
    }

    #[test]
    fn a_span_of_one_line_links_to_the_line_alone() {
        let origin = origin();

        assert_eq!(
            origin.blob_url("abc123", "src/ui.rs", Some((7, 7))),
            "https://github.com/tale/prtui/blob/abc123/src/ui.rs#L7"
        );
        assert_eq!(
            origin.blob_url("abc123", "src/ui.rs", Some((7, 9))),
            "https://github.com/tale/prtui/blob/abc123/src/ui.rs#L7-L9"
        );
        assert_eq!(
            origin.blob_url("abc123", "src/ui.rs", None),
            "https://github.com/tale/prtui/blob/abc123/src/ui.rs"
        );
    }

    #[test]
    fn the_pull_request_and_its_comments_share_a_page() {
        let origin = origin();

        assert_eq!(origin.pull_url(), "https://github.com/tale/prtui/pull/42");
        assert_eq!(
            origin.comment_url(918),
            "https://github.com/tale/prtui/pull/42#discussion_r918"
        );
    }
}
