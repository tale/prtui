//! Opaque identifiers handed across the provider boundary.

use anyhow::{Context, Result, bail};
use std::fmt::{self, Display};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentRef {
    Published { mr: u32, note: u64 },
    Draft { mr: u32, draft: u64 },
}

impl CommentRef {
    pub fn parse(id: &str) -> Result<Self> {
        let (mr, rest) = id
            .split_once(':')
            .with_context(|| format!("malformed GitLab comment id {id:?}"))?;
        let mr = mr
            .parse()
            .with_context(|| format!("malformed GitLab comment id {id:?}"))?;

        let value = |text: &str| -> Result<u64> {
            text.parse()
                .with_context(|| format!("malformed GitLab comment id {id:?}"))
        };

        if let Some(note) = rest.strip_prefix('n') {
            return Ok(Self::Published {
                mr,
                note: value(note)?,
            });
        }

        let Some(draft) = rest.strip_prefix('d') else {
            bail!("malformed GitLab comment id {id:?}");
        };

        Ok(Self::Draft {
            mr,
            draft: value(draft)?,
        })
    }
}

impl Display for CommentRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Published { mr, note } => write!(f, "{mr}:n{note}"),
            Self::Draft { mr, draft } => write!(f, "{mr}:d{draft}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadRef {
    pub mr: u32,
    pub discussion: String,
}

impl ThreadRef {
    pub fn parse(id: &str) -> Result<Self> {
        let (mr, discussion) = id
            .split_once(':')
            .with_context(|| format!("malformed GitLab thread id {id:?}"))?;

        if discussion.is_empty() {
            bail!("malformed GitLab thread id {id:?}");
        }

        Ok(Self {
            mr: mr.parse().with_context(|| {
                format!("malformed GitLab thread id {id:?}")
            })?,
            discussion: discussion.to_owned(),
        })
    }
}

impl Display for ThreadRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.mr, self.discussion)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyRef {
    pub discussion: String,
    pub note: u64,
}

impl ReplyRef {
    pub fn parse(id: &str) -> Result<Self> {
        let (discussion, note) = id
            .rsplit_once(':')
            .with_context(|| format!("malformed GitLab reply id {id:?}"))?;

        if discussion.is_empty() {
            bail!("malformed GitLab reply id {id:?}");
        }

        Ok(Self {
            discussion: discussion.to_owned(),
            note: note
                .parse()
                .with_context(|| format!("malformed GitLab reply id {id:?}"))?,
        })
    }

    pub fn note_anchor(id: &str) -> &str {
        id.rsplit_once(':').map_or(id, |(_, note)| note)
    }
}

impl Display for ReplyRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.discussion, self.note)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_comment_round_trips_through_its_opaque_form() {
        let published = CommentRef::Published { mr: 7, note: 42 };
        assert_eq!(published.to_string(), "7:n42");
        assert_eq!(CommentRef::parse("7:n42").unwrap(), published);

        let draft = CommentRef::Draft { mr: 7, draft: 3 };
        assert_eq!(draft.to_string(), "7:d3");
        assert_eq!(CommentRef::parse("7:d3").unwrap(), draft);
    }

    #[test]
    fn a_thread_keeps_its_hex_discussion_id() {
        let thread = ThreadRef {
            mr: 1,
            discussion: "fc9615a242ccf8160400c6bc0dbee26eb15f1302".to_owned(),
        };

        assert_eq!(
            thread.to_string(),
            "1:fc9615a242ccf8160400c6bc0dbee26eb15f1302"
        );
        assert_eq!(ThreadRef::parse(&thread.to_string()).unwrap(), thread);
    }

    #[test]
    fn a_reply_splits_off_only_the_trailing_note() {
        let reply = ReplyRef {
            discussion: "abc123".to_owned(),
            note: 5,
        };

        assert_eq!(reply.to_string(), "abc123:5");
        assert_eq!(ReplyRef::parse("abc123:5").unwrap(), reply);
        assert_eq!(ReplyRef::note_anchor("abc123:5"), "5");
    }

    #[test]
    fn a_malformed_identifier_is_reported_rather_than_guessed() {
        for id in ["", "7", "7:x42", "seven:n42", "7:n", ":abc"] {
            assert!(CommentRef::parse(id).is_err(), "{id}");
        }

        assert!(ThreadRef::parse("1:").is_err());
        assert!(ReplyRef::parse("abc123:nope").is_err());
    }
}
