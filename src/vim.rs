//! The motion vocabulary every surface reads keys through.
//!
//! The review app and the pull request selector bind the same chords to the
//! same [`Motion`], so the cursor arithmetic, the scroll-off margin and the
//! search wrap live here rather than once per surface.

/// Cursor movements, expressed independently of which pane owns the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Down(usize),
    Up(usize),
    HalfPageDown,
    HalfPageUp,
    Top,
    Bottom,
    /// A line by the number the gutter shows, which is the new side of the
    /// diff, or a row of the tree when the files pane has the cursor.
    Line(usize),
}

/// Rows of context kept between the cursor and the edge of the viewport, and
/// never more than a quarter of a short one.
const SCROLL_OFF: usize = 3;

/// Resolves a motion to an absolute position in a list of `len` items.
pub fn step(
    motion: Motion,
    current: usize,
    len: usize,
    viewport: usize,
) -> usize {
    let last = len.saturating_sub(1);

    match motion {
        Motion::Down(n) => current.saturating_add(n).min(last),
        Motion::Up(n) => current.saturating_sub(n),
        Motion::HalfPageDown => current.saturating_add(viewport / 2).min(last),
        Motion::HalfPageUp => current.saturating_sub(viewport / 2),
        Motion::Top => 0,
        Motion::Bottom => last,
        Motion::Line(number) => number.saturating_sub(1).min(last),
    }
}

/// Vim's `n` and `N` over a list of hits, which wrap at both ends.
///
/// `current` is the hit the reader is standing on, if any; a search that has
/// not landed yet starts at the end it is stepping away from.
pub const fn step_hit(
    current: Option<usize>,
    len: usize,
    direction: isize,
    count: usize,
) -> Option<usize> {
    if len == 0 {
        return None;
    }

    let mut index = current;
    let mut remaining = count;

    while remaining > 0 {
        index = Some(match index {
            Some(index) if direction > 0 => (index + 1) % len,
            Some(index) => (index + len - 1) % len,
            None if direction > 0 => 0,
            None => len - 1,
        });
        remaining -= 1;
    }

    index
}

/// A cursor into a list, and where the list is scrolled to under it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub index: usize,
    pub scroll: usize,
}

impl Cursor {
    pub const fn at(index: usize) -> Self {
        Self { index, scroll: 0 }
    }

    pub fn apply(&mut self, motion: Motion, len: usize, viewport: usize) {
        self.jump(step(motion, self.index, len, viewport), len, viewport);
    }

    pub fn jump(&mut self, index: usize, len: usize, viewport: usize) {
        self.index = index.min(len.saturating_sub(1));
        self.follow(len, viewport);
    }

    /// Keeps the cursor inside the viewport with a scroll-off margin, and the
    /// viewport inside the list.
    pub fn follow(&mut self, len: usize, viewport: usize) {
        if viewport == 0 {
            return;
        }

        let margin = SCROLL_OFF.min(viewport / 4);
        let bottom = self.scroll + viewport.saturating_sub(margin + 1);

        if self.index < self.scroll + margin {
            self.scroll = self.index.saturating_sub(margin);
        } else if self.index > bottom {
            self.scroll = (self.index + margin + 1).saturating_sub(viewport);
        }

        self.scroll = self.scroll.min(len.saturating_sub(viewport));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_count_names_the_line_rather_than_the_repeat() {
        assert_eq!(step(Motion::Line(42), 0, 100, 20), 41);
        assert_eq!(step(Motion::Line(999), 0, 100, 20), 99);
        assert_eq!(step(Motion::Down(7), 0, 100, 20), 7);
    }

    #[test]
    fn every_motion_stays_inside_the_list() {
        assert_eq!(step(Motion::Up(9), 3, 100, 20), 0);
        assert_eq!(step(Motion::HalfPageDown, 95, 100, 20), 99);
        assert_eq!(step(Motion::Bottom, 0, 0, 20), 0);
    }

    #[test]
    fn the_cursor_scrolls_only_once_it_reaches_the_margin() {
        let mut cursor = Cursor::default();

        cursor.apply(Motion::Down(10), 100, 20);
        assert_eq!(cursor.scroll, 0);

        cursor.apply(Motion::Down(10), 100, 20);
        assert_eq!(cursor.index, 20);
        assert_eq!(cursor.scroll, 4);
    }

    #[test]
    fn the_last_page_is_not_scrolled_past() {
        let mut cursor = Cursor::default();
        cursor.apply(Motion::Bottom, 30, 20);

        assert_eq!(cursor.index, 29);
        assert_eq!(cursor.scroll, 10);
    }

    #[test]
    fn hits_wrap_at_both_ends() {
        assert_eq!(step_hit(None, 3, 1, 1), Some(0));
        assert_eq!(step_hit(None, 3, -1, 1), Some(2));
        assert_eq!(step_hit(Some(2), 3, 1, 1), Some(0));
        assert_eq!(step_hit(Some(0), 3, -1, 1), Some(2));
        assert_eq!(step_hit(Some(0), 3, 1, 4), Some(1));
        assert_eq!(step_hit(Some(0), 0, 1, 1), None);
    }
}
