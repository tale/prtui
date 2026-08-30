//! Pulling the rest of the file into a diff.
//!
//! A patch carries only the lines around each change, so the file it describes
//! arrives with holes in it: above the first hunk, between hunks, and below the
//! last. A [`Gap`] is one of those holes, and [`reveal`] fills part of one back
//! in from the file at head — the same thing github.com does when it offers to
//! expand a run of hidden lines.
//!
//! A gap holds only unchanged lines, so both sides of the diff read the same
//! text out of it and the two line numberings stay a fixed distance apart.

use crate::model::{ChangedFile, DiffLine, LineKind, parse_hunk_header};

/// Lines one step of an expansion pulls in, matching what github.com reveals.
pub const STEP: u32 = 20;

/// Which hunks a gap sits between, which is what decides the ways it can open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// Above the first hunk, so it opens upward only: there is no hunk over it
    /// for a downward reveal to extend.
    Leading,
    Between,
    /// Below the last hunk, so it opens downward only, and it ends where the
    /// file ends rather than where the next hunk starts.
    Trailing,
}

/// A run of the file the patch left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    pub place: Place,
    /// Index of the hunk header the gap sits above, which is the row that
    /// stands for it. The trailing gap names one past the last line of the
    /// patch, where no header follows it.
    pub at: usize,
    /// First line the gap hides, on each side of the diff.
    pub old_start: u32,
    pub new_start: u32,
    /// Lines the gap hides, or `None` for the trailing gap, whose length is
    /// not knowable from the patch alone.
    pub len: Option<u32>,
}

impl Gap {
    /// How many lines the gap really hides, given a file of `total` lines.
    pub fn len_in(&self, total: usize) -> u32 {
        self.len.unwrap_or_else(|| {
            (total as u32 + 1).saturating_sub(self.new_start)
        })
    }
}

/// What one reveal put into the patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Revealed {
    pub count: usize,
    /// Index the lines were spliced in at. Everything from here down moved,
    /// which is what a cursor or a scroll offset has to be carried by.
    pub at: usize,
}

/// How much of a gap to pull in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reveal {
    /// The lines nearest the hunk below, extending it upward.
    Up(u32),
    /// The lines nearest the hunk above, extending it downward.
    Down(u32),
    /// Everything the gap hides, which closes it.
    All,
}

/// Where one hunk starts, and where the line after it would be, on each side.
struct Hunk {
    at: usize,
    old_start: u32,
    new_start: u32,
    old_end: u32,
    new_end: u32,
}

fn hunks(lines: &[DiffLine]) -> Vec<Hunk> {
    let capacity = lines
        .iter()
        .filter(|line| line.kind == LineKind::Hunk)
        .count();
    let mut hunks = Vec::with_capacity(capacity);

    for (index, line) in lines.iter().enumerate() {
        if line.kind == LineKind::Hunk {
            let Some((old_start, new_start)) = parse_hunk_header(&line.text)
            else {
                continue;
            };

            hunks.push(Hunk {
                at: index,
                old_start,
                new_start,
                old_end: old_start,
                new_end: new_start,
            });
            continue;
        }

        let Some(hunk) = hunks.last_mut() else {
            continue;
        };

        if let Some(old) = line.old_line {
            hunk.old_end = old + 1;
        }
        if let Some(new) = line.new_line {
            hunk.new_end = new + 1;
        }
    }

    hunks
}

/// Every run of the file the patch left out, in the order it would be read.
///
/// A patch with no hunks describes nothing — a binary file, or one GitHub sent
/// without a diff — so it has no gaps rather than one enormous one.
pub fn gaps(file: &ChangedFile) -> Vec<Gap> {
    let hunks = hunks(&file.lines);
    let (Some(first), Some(last)) = (hunks.first(), hunks.last()) else {
        return Vec::new();
    };

    let mut gaps = Vec::new();

    // Both numberings walk up from the first hunk together, and the gap ends
    // wherever the shorter of the two runs out.
    let leading = first.new_start.min(first.old_start).saturating_sub(1);
    if leading > 0 {
        gaps.push(Gap {
            place: Place::Leading,
            at: first.at,
            old_start: first.old_start - leading,
            new_start: first.new_start - leading,
            len: Some(leading),
        });
    }

    for pair in hunks.windows(2) {
        let (above, below) = (&pair[0], &pair[1]);
        let len = below
            .new_start
            .saturating_sub(above.new_end)
            .min(below.old_start.saturating_sub(above.old_end));

        if len == 0 {
            continue;
        }

        gaps.push(Gap {
            place: Place::Between,
            at: below.at,
            old_start: above.old_end,
            new_start: above.new_end,
            len: Some(len),
        });
    }

    gaps.push(Gap {
        place: Place::Trailing,
        at: file.lines.len(),
        old_start: last.old_end,
        new_start: last.new_end,
        len: None,
    });

    gaps
}

/// Splices part of `gap` into the patch out of `content`, the file at head
/// split into lines. Answers with nothing when the gap does not open that way,
/// or holds no more lines to give.
///
/// The patch stays a patch: the revealed lines land inside a hunk rather than
/// beside it, the header of a gap that closes goes away, and every header left
/// is renumbered to describe what now sits under it.
pub fn reveal(
    file: &mut ChangedFile,
    gap: &Gap,
    how: Reveal,
    content: &[String],
) -> Option<Revealed> {
    let total = gap.len_in(content.len());
    let count = match how {
        Reveal::All => total,
        Reveal::Up(lines) | Reveal::Down(lines) => lines.min(total),
    };

    if count == 0 || !is_open(gap, how) {
        return None;
    }

    // An upward reveal takes the end of the gap, a downward one its start.
    let is_up = matches!(how, Reveal::Up(_))
        || (matches!(how, Reveal::All) && gap.place == Place::Leading);
    let first = if is_up {
        gap.new_start + total - count
    } else {
        gap.new_start
    };

    let revealed: Vec<DiffLine> = (first..first + count)
        .map_while(|new| {
            let text = content.get(new as usize - 1)?;
            Some(DiffLine {
                kind: LineKind::Context,
                text: text.clone(),
                old_line: Some(new - gap.new_start + gap.old_start),
                new_line: Some(new),
            })
        })
        .collect();

    if revealed.is_empty() {
        return None;
    }

    // A gap the reveal exhausts hides nothing left to announce, so its header
    // goes with it. The lines of an upward reveal that leaves the gap open lead
    // the hunk below instead, which puts them under that header rather than
    // over it.
    let count = revealed.len();
    let closes = count as u32 == total && closable(gap);
    let at = if closes || !is_up { gap.at } else { gap.at + 1 };

    file.lines.splice(at..at, revealed);

    if closes {
        file.lines.remove(at + count);
    }

    renumber_headers(&mut file.lines);

    Some(Revealed { count, at })
}

/// Whether the gap opens the way `how` asks. A gap at either end of the patch
/// has a hunk on one side only, and can be pulled apart from that side alone.
const fn is_open(gap: &Gap, how: Reveal) -> bool {
    match how {
        Reveal::All => true,
        Reveal::Up(_) => !matches!(gap.place, Place::Trailing),
        Reveal::Down(_) => !matches!(gap.place, Place::Leading),
    }
}

/// Whether closing the gap should take its header with it. The trailing gap
/// has no header, and a leading one that does not reach line 1 still hides the
/// lines a truncated patch disagreed about.
const fn closable(gap: &Gap) -> bool {
    match gap.place {
        Place::Leading => gap.new_start == 1 && gap.old_start == 1,
        Place::Between => true,
        Place::Trailing => false,
    }
}

/// Rewrites every hunk header so its line counts describe what sits under it.
///
/// A revealed run lands inside a hunk, so the numbers the header was drawn with
/// stop being true the moment anything is spliced in.
fn renumber_headers(lines: &mut [DiffLine]) {
    let headers: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.kind == LineKind::Hunk)
        .map(|(index, _)| index)
        .collect();

    for (position, &at) in headers.iter().enumerate() {
        let end = headers.get(position + 1).copied().unwrap_or(lines.len());
        let body = &lines[at + 1..end];

        let (old_start, old_count) = span(body, |line| line.old_line);
        let (new_start, new_count) = span(body, |line| line.new_line);
        let section = section(&lines[at].text).to_string();

        lines[at].text = format!(
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@{section}"
        );
    }
}

/// The first line number and the line count on one side of a hunk. A side the
/// hunk does not touch starts at zero, which is how a patch says so.
fn span(
    body: &[DiffLine],
    number: impl Fn(&DiffLine) -> Option<u32>,
) -> (u32, u32) {
    let mut start = None;
    let mut count = 0;

    for line in body {
        let Some(line) = number(line) else {
            continue;
        };

        start.get_or_insert(line);
        count += 1;
    }

    (start.unwrap_or(0), count)
}

/// The declaration github.com appends after the second `@@`, kept as it came.
fn section(header: &str) -> &str {
    header.split_once(" @@").map_or("", |(_, rest)| rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A patch against a 41-line file: one hunk around line 10 that adds a
    /// line, another around line 31, and a run of untouched lines everywhere
    /// else for a reveal to pull in.
    const PATCH: &str = "@@ -10,3 +10,4 @@ fn ten()\n \
        line 10\n line 11\n+added\n line 12\n\
        @@ -30,2 +31,2 @@ fn thirty()\n line 30\n line 31";

    fn file() -> ChangedFile {
        let page = serde_json::json!([[{
            "filename": "src/main.rs",
            "status": "modified",
            "additions": 1,
            "deletions": 0,
            "patch": PATCH,
        }]]);

        crate::model::parse_files(&page).unwrap().remove(0)
    }

    /// The file as head has it, which is what a reveal reads out of.
    fn head() -> Vec<String> {
        (1..=41).map(|line| format!("head {line}")).collect()
    }

    fn headers(file: &ChangedFile) -> Vec<&str> {
        file.lines
            .iter()
            .filter(|line| line.kind == LineKind::Hunk)
            .map(|line| line.text.as_str())
            .collect()
    }

    fn revealed(file: &mut ChangedFile, gap: &Gap, how: Reveal) -> usize {
        reveal(file, gap, how, &head()).unwrap().count
    }

    fn texts(file: &ChangedFile) -> Vec<&str> {
        file.lines.iter().map(|line| line.text.as_str()).collect()
    }

    #[test]
    fn a_gap_is_every_run_the_patch_left_out() {
        let gaps = gaps(&file());

        assert_eq!(
            gaps,
            [
                Gap {
                    place: Place::Leading,
                    at: 0,
                    old_start: 1,
                    new_start: 1,
                    len: Some(9),
                },
                Gap {
                    place: Place::Between,
                    at: 5,
                    old_start: 13,
                    new_start: 14,
                    len: Some(17),
                },
                Gap {
                    place: Place::Trailing,
                    at: 8,
                    old_start: 32,
                    new_start: 33,
                    len: None,
                },
            ]
        );

        // Only the trailing gap needs the file to say how long it is.
        assert_eq!(gaps[1].len_in(41), 17);
        assert_eq!(gaps[2].len_in(41), 9);
    }

    /// A patch with nothing in it — a binary file, or one GitHub sent without
    /// a diff — describes no file, so there is nothing to expand into.
    #[test]
    fn a_patch_with_no_hunks_has_no_gaps() {
        let mut empty = file();
        empty.lines.clear();

        assert!(gaps(&empty).is_empty());
    }

    #[test]
    fn an_upward_reveal_leads_the_hunk_below_it() {
        let mut file = file();
        let gap = gaps(&file)[0];

        assert_eq!(revealed(&mut file, &gap, Reveal::Up(3)), 3);

        // The revealed lines sit under the header, since they are the hunk's
        // leading context now, and the header says so.
        assert_eq!(headers(&file)[0], "@@ -7,6 +7,7 @@ fn ten()");
        assert_eq!(&texts(&file)[1..4], ["head 7", "head 8", "head 9"]);
        assert_eq!(file.lines[1].old_line, Some(7));
        assert_eq!(file.lines[1].new_line, Some(7));

        // What is left of the gap still hangs off the same header.
        assert_eq!(gaps(&file)[0].len, Some(6));
    }

    #[test]
    fn a_downward_reveal_trails_the_hunk_above_it() {
        let mut file = file();
        let gap = gaps(&file)[1];

        assert_eq!(revealed(&mut file, &gap, Reveal::Down(4)), 4);

        // They extend the first hunk, so they land above the second header.
        assert_eq!(
            headers(&file),
            [
                "@@ -10,7 +10,8 @@ fn ten()",
                "@@ -30,2 +31,2 @@ fn thirty()"
            ]
        );
        assert_eq!(
            &texts(&file)[5..9],
            ["head 14", "head 15", "head 16", "head 17"]
        );
        assert_eq!(file.lines[5].old_line, Some(13));

        assert_eq!(gaps(&file)[1].len, Some(13));
    }

    /// A header stands for the lines hidden under it. Reveal all of them and
    /// it has nothing left to say, so the two hunks read as one.
    #[test]
    fn a_gap_the_reveal_exhausts_takes_its_header_with_it() {
        let mut file = file();
        let gap = gaps(&file)[1];

        assert_eq!(revealed(&mut file, &gap, Reveal::All), 17);
        assert_eq!(headers(&file), ["@@ -10,22 +10,23 @@ fn ten()"]);

        // Only the two ends are left to open.
        let gaps = gaps(&file);
        assert_eq!(gaps.len(), 2);
        assert_eq!(gaps[0].place, Place::Leading);
        assert_eq!(gaps[1].place, Place::Trailing);
    }

    #[test]
    fn reaching_the_top_of_the_file_leaves_no_header_over_it() {
        let mut file = file();
        let gap = gaps(&file)[0];

        assert_eq!(revealed(&mut file, &gap, Reveal::All), 9);
        assert_eq!(texts(&file)[0], "head 1");
        assert_eq!(headers(&file), ["@@ -30,2 +31,2 @@ fn thirty()"]);
    }

    #[test]
    fn the_trailing_gap_ends_where_the_file_does() {
        let mut file = file();
        let gap = gaps(&file)[2];

        assert_eq!(revealed(&mut file, &gap, Reveal::All), 9);
        assert_eq!(texts(&file).last(), Some(&"head 41"));
        // Nothing follows it, so its hunk keeps the header it always had.
        assert_eq!(headers(&file)[1], "@@ -30,11 +31,11 @@ fn thirty()");

        // Asking again reveals nothing rather than running off the end.
        let gap = gaps(&file).pop().unwrap();
        assert!(reveal(&mut file, &gap, Reveal::All, &head()).is_none());
    }

    #[test]
    fn a_gap_opens_only_towards_a_hunk_it_has() {
        let mut file = file();
        let [leading, _, trailing] = gaps(&file)[..] else {
            panic!("expected three gaps");
        };

        assert!(
            reveal(&mut file, &leading, Reveal::Down(3), &head()).is_none()
        );
        assert!(reveal(&mut file, &trailing, Reveal::Up(3), &head()).is_none());
        assert_eq!(file.lines.len(), 8);
    }

    /// The file on disk is the authority on where it ends. A patch that claims
    /// lines past it reveals what is there and stops.
    #[test]
    fn a_reveal_stops_at_the_end_of_what_head_holds() {
        let mut file = file();
        let gap = gaps(&file)[2];
        let short: Vec<String> = head().into_iter().take(35).collect();

        assert_eq!(
            reveal(&mut file, &gap, Reveal::All, &short).unwrap().count,
            3
        );
        assert_eq!(texts(&file).last(), Some(&"head 35"));
    }
}
