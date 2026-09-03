//! The file pane's virtual row model.
//!
//! The pane is a tree of the changed files, so a row is either a directory
//! heading or a file under one. Building that list up front is what lets the
//! cursor, the scroll offset and the renderer address the same thing, the way
//! [`super::rows`] does for the diff.
//!
//! A chain of directories with nothing else in it collapses into one heading:
//! `pkg/cmd/attestation/verify/` is a row, not four. Without that, a deep path
//! spends more columns on indentation than the tree saves by grouping.

use prtui_core::ChangedFile;
use std::borrow::Borrow;
use std::collections::HashSet;
use std::sync::Arc;

/// Columns each level of nesting indents by. One: the tree is shallow once
/// unbranching chains are folded into a single heading, and a wider step buys
/// nothing but a shorter filename.
pub const INDENT: usize = 1;

/// One row of the file pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// A directory heading. `path` carries its trailing slash and is what
    /// collapse state is keyed on; `label` is the run of segments this one row
    /// stands for, which is several when the chain had no branch in it.
    Directory {
        path: Arc<str>,
        label: String,
        depth: usize,
        /// Files underneath, at any depth, so a collapsed row still says how
        /// much it is hiding.
        files: usize,
        /// Open conversations under it, for the same reason: folding a
        /// directory must not fold away the thing the reader is looking for.
        unresolved: usize,
        is_collapsed: bool,
    },
    /// A changed file, by index into [`crate::app::App::files`].
    File { index: usize, depth: usize },
}

impl Row {
    pub const fn depth(&self) -> usize {
        match self {
            Self::Directory { depth, .. } | Self::File { depth, .. } => *depth,
        }
    }

    pub const fn file(&self) -> Option<usize> {
        match self {
            Self::File { index, .. } => Some(*index),
            Self::Directory { .. } => None,
        }
    }
}

pub struct Tree {
    all: Vec<Row>,
    /// First row on screen, kept here so the renderer slices rather than
    /// deciding.
    start: usize,
    /// A directory every changed file is under, lifted out of the rows and into
    /// the pane's title. A review usually touches one area, so this is the
    /// common case, and paying an indent level plus a row to say `pkg/` four
    /// times over is what makes a tree cost more room than a flat list.
    root: Option<Arc<str>>,
}

impl Tree {
    pub const fn empty() -> Self {
        Self {
            all: Vec::new(),
            start: 0,
            root: None,
        }
    }

    /// Lays `visible` out as a tree.
    ///
    /// `collapsed` names directories the reader has folded away. A filter
    /// overrides them: a file that matched has to be reachable, and reaching it
    /// by hand through a fold it did not open is not navigation.
    pub fn build<F>(
        files: &[F],
        visible: &[usize],
        collapsed: &HashSet<Arc<str>>,
        is_filtered: bool,
        unresolved: &[usize],
    ) -> Self
    where
        F: Borrow<ChangedFile>,
    {
        let mut sorted: Vec<usize> = visible.to_vec();
        sorted.sort_by(|&a, &b| {
            files[a].borrow().path.cmp(&files[b].borrow().path)
        });

        let mut builder = Builder {
            files,
            collapsed,
            is_filtered,
            unresolved,
            rows: Vec::with_capacity(sorted.len()),
        };
        builder.emit(&sorted, 0, 0);

        let mut tree = Self {
            all: builder.rows,
            start: 0,
            root: None,
        };
        tree.hoist_root();
        tree
    }

    /// Lifts a heading that holds everything into [`Self::root`].
    ///
    /// Only when it is the sole row at the outermost level: otherwise there is
    /// no shared directory to name, and folding it away would hide the tree.
    fn hoist_root(&mut self) {
        let is_only_root = self
            .all
            .iter()
            .filter(|row| row.depth() == 0)
            .try_fold(false, |seen, row| match row {
                Row::Directory { is_collapsed, .. }
                    if !seen && !is_collapsed =>
                {
                    Some(true)
                }
                _ => None,
            })
            .unwrap_or(false);

        if !is_only_root {
            return;
        }

        let Some(Row::Directory { path, .. }) = self.all.first() else {
            return;
        };

        self.root = Some(path.clone());
        self.all.remove(0);

        for row in &mut self.all {
            match row {
                Row::Directory { depth, .. } | Row::File { depth, .. } => {
                    *depth -= 1;
                }
            }
        }
    }

    /// The directory every file is under, when they share one.
    pub fn root(&self) -> Option<&str> {
        self.root.as_deref()
    }

    /// Scrolls so `row` is on screen, keeping it centred until the list runs
    /// out at either end.
    pub fn focus(&mut self, row: usize, height: usize) {
        self.start = row
            .saturating_sub(height / 2)
            .min(self.all.len().saturating_sub(height));
    }

    pub fn rows(&self) -> &[Row] {
        &self.all
    }

    pub const fn len(&self) -> usize {
        self.all.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.all.is_empty()
    }

    pub fn get(&self, row: usize) -> Option<&Row> {
        self.all.get(row)
    }

    /// The rows this frame has room for, and the offset they start at.
    pub fn window(&self, height: usize) -> &[Row] {
        let end = self.start.saturating_add(height).min(self.all.len());

        &self.all[self.start.min(end)..end]
    }

    pub const fn start(&self) -> usize {
        self.start
    }

    /// Where a file sits in the row list, when it is not folded away.
    pub fn row_of(&self, index: usize) -> Option<usize> {
        self.all.iter().position(|row| row.file() == Some(index))
    }

    /// Where a directory heading sits in the row list.
    pub fn row_of_directory(&self, path: &str) -> Option<usize> {
        self.all.iter().position(
            |row| matches!(row, Row::Directory { path: own, .. } if &**own == path),
        )
    }

    /// Files in the order the tree lists them, which is the order `]` and `[`
    /// walk. Sorting by path is what makes the flat order agree with the tree.
    pub fn files(&self) -> impl Iterator<Item = usize> + '_ {
        self.all.iter().filter_map(Row::file)
    }

    pub fn file_count(&self) -> usize {
        self.files().count()
    }

    /// Where a file sits among the tree's files, counting from one. Headings
    /// are not counted: the status bar is reporting files, not rows.
    pub fn file_position(&self, index: usize) -> usize {
        self.files()
            .position(|candidate| candidate == index)
            .map_or(0, |position| position + 1)
    }

    /// The nearest heading above `row`, which is what a fold key acts on when
    /// the cursor is on a file rather than a heading.
    pub fn enclosing_directory(&self, row: usize) -> Option<&Arc<str>> {
        let depth = self.all.get(row)?.depth();

        self.all[..row]
            .iter()
            .rev()
            .find_map(|candidate| match candidate {
                Row::Directory {
                    path, depth: own, ..
                } if *own < depth => Some(path),
                _ => None,
            })
    }
}

struct Builder<'a, F> {
    files: &'a [F],
    collapsed: &'a HashSet<Arc<str>>,
    is_filtered: bool,
    /// Open conversations per file, parallel to `files`.
    unresolved: &'a [usize],
    rows: Vec<Row>,
}

impl<F> Builder<'_, F>
where
    F: Borrow<ChangedFile>,
{
    fn file(&self, index: usize) -> &ChangedFile {
        self.files[index].borrow()
    }

    /// Lays out one directory's contents. `group` is sorted and every path in
    /// it shares the first `prefix` bytes, which is the directory itself.
    fn emit(&mut self, group: &[usize], prefix: usize, depth: usize) {
        let mut at = 0;

        while at < group.len() {
            let path = &*self.file(group[at]).path;
            let Some(separator) = path[prefix..].find('/') else {
                self.rows.push(Row::File {
                    index: group[at],
                    depth,
                });
                at += 1;
                continue;
            };

            let end = prefix + separator + 1;
            let segment = &path[prefix..end];
            let run = group[at..]
                .iter()
                .take_while(|&&index| {
                    self.file(index).path[prefix..].starts_with(segment)
                })
                .count();

            self.emit_directory(&group[at..at + run], prefix, end, depth);
            at += run;
        }
    }

    /// A directory and everything under it. The heading absorbs any further
    /// directories the whole group also shares, so an unbranching chain reads
    /// as one row rather than one indent per level.
    fn emit_directory(
        &mut self,
        group: &[usize],
        prefix: usize,
        mut end: usize,
        depth: usize,
    ) {
        while let Some(next) = self.shared_segment(group, end) {
            end = next;
        }

        let path: Arc<str> = Arc::from(&self.file(group[0]).path[..end]);
        let is_collapsed = !self.is_filtered && self.collapsed.contains(&path);

        self.rows.push(Row::Directory {
            label: self.file(group[0]).path[prefix..end].to_string(),
            path,
            depth,
            files: group.len(),
            unresolved: group
                .iter()
                .map(|&index| {
                    self.unresolved.get(index).copied().unwrap_or_default()
                })
                .sum(),
            is_collapsed,
        });

        if !is_collapsed {
            self.emit(group, end, depth + 1);
        }
    }

    /// Where the next directory segment ends, when every path in `group` is
    /// inside it. A group of one is still a chain: a lone deep file reads
    /// better as one heading than as a ladder.
    fn shared_segment(&self, group: &[usize], end: usize) -> Option<usize> {
        let path = &*self.file(group[0]).path;
        let separator = path[end..].find('/')?;
        let next = end + separator + 1;
        let segment = &path[end..next];

        group[1..]
            .iter()
            .all(|&index| self.file(index).path[end..].starts_with(segment))
            .then_some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn file(path: &str) -> ChangedFile {
        ChangedFile {
            path: Arc::from(path),
            status: "modified".into(),
            additions: 0,
            deletions: 0,
            lines: Vec::new(),
        }
    }

    fn tree_of(paths: &[&str], collapsed: &[&str]) -> Tree {
        let files: Vec<ChangedFile> = paths.iter().copied().map(file).collect();
        let visible: Vec<usize> = (0..files.len()).collect();
        let folded: HashSet<Arc<str>> =
            collapsed.iter().map(|path| Arc::from(*path)).collect();

        Tree::build(&files, &visible, &folded, false, &[])
    }

    fn build(paths: &[&str], collapsed: &[&str]) -> Vec<String> {
        tree_of(paths, collapsed)
            .rows()
            .iter()
            .map(|row| match row {
                Row::Directory { label, depth, .. } => {
                    format!("{}{label}", " ".repeat(depth * INDENT))
                }
                Row::File { index, depth } => format!(
                    "{}{}",
                    " ".repeat(depth * INDENT),
                    paths[*index].rsplit('/').next().unwrap()
                ),
            })
            .collect()
    }

    /// The whole chain is shared, so it names the pane and the file sits at the
    /// top level rather than four indents deep.
    #[test]
    fn an_unbranching_chain_is_one_heading() {
        let paths = ["pkg/cmd/attestation/verify/verify.go"];

        assert_eq!(build(&paths, &[]), ["verify.go"]);
        assert_eq!(
            tree_of(&paths, &[]).root(),
            Some("pkg/cmd/attestation/verify/")
        );
    }

    #[test]
    fn a_chain_stops_where_it_branches() {
        let paths = [
            "pkg/cmd/attestation/verify/verify.go",
            "pkg/cmdutil/auth_check.go",
        ];

        assert_eq!(
            build(&paths, &[]),
            [
                "cmd/attestation/verify/",
                " verify.go",
                "cmdutil/",
                " auth_check.go",
            ]
        );
        assert_eq!(tree_of(&paths, &[]).root(), Some("pkg/"));
    }

    #[test]
    fn a_file_beside_a_directory_keeps_its_level() {
        assert_eq!(
            build(&["src/lib.rs", "src/app/mod.rs"], &[]),
            ["app/", " mod.rs", "lib.rs"]
        );
    }

    /// Two roots share no directory, so there is nothing to lift out.
    #[test]
    fn nothing_is_hoisted_when_the_files_share_no_directory() {
        let paths = ["src/lib.rs", "tests/render.rs"];

        assert_eq!(
            build(&paths, &[]),
            ["src/", " lib.rs", "tests/", " render.rs"]
        );
        assert_eq!(tree_of(&paths, &[]).root(), None);
    }

    #[test]
    fn a_collapsed_directory_hides_what_is_under_it() {
        assert_eq!(
            build(
                &["src/app/mod.rs", "src/app/draft.rs", "src/lib.rs"],
                &["src/app/"]
            ),
            ["app/", "lib.rs"]
        );
    }

    /// Hoisting a folded row would leave an empty pane with no way back, so the
    /// row everything is under stays a row once it is folded.
    #[test]
    fn a_collapsed_root_stays_a_row() {
        assert_eq!(
            build(&["src/app/mod.rs", "src/app/draft.rs"], &["src/app/"]),
            ["src/app/"]
        );
    }

    #[test]
    fn a_filter_overrides_every_fold() {
        let files: Vec<ChangedFile> =
            ["src/app/mod.rs", "src/lib.rs"].map(file).into();
        let folded: HashSet<Arc<str>> =
            std::iter::once(Arc::from("src/app/")).collect();

        let tree = Tree::build(&files, &[0, 1], &folded, true, &[]);
        assert_eq!(tree.files().collect::<Vec<_>>(), [0, 1]);
    }

    #[test]
    fn a_fold_key_finds_the_heading_above_the_cursor() {
        let files: Vec<ChangedFile> =
            ["src/app/mod.rs", "src/lib.rs"].map(file).into();
        let tree = Tree::build(&files, &[0, 1], &HashSet::new(), false, &[]);

        // `src/` names the pane, so the rows are: app/, mod.rs, lib.rs.
        assert_eq!(
            tree.enclosing_directory(1).map(|path| &**path),
            Some("src/app/")
        );
        // A file at the top level sits under the hoisted root, which is not a
        // row and so cannot be folded.
        assert!(tree.enclosing_directory(2).is_none());
        assert!(tree.enclosing_directory(0).is_none());
    }

    #[test]
    fn files_come_back_in_the_order_the_tree_lists_them() {
        let paths = ["z/last.rs", "a/first.rs"];
        let files: Vec<ChangedFile> = paths.map(file).into();
        let tree = Tree::build(&files, &[0, 1], &HashSet::new(), false, &[]);

        assert_eq!(tree.files().collect::<Vec<_>>(), [1, 0]);
    }
}
