use ratatui::style::Color;

/// The terminal background class used by both syntax and diff rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    Dark,
    Light,
}

/// One coherent palette for the whole renderer. Syntax colors come from a
/// matching syntect theme; these colors cover diff state and lightweight UI.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub mode: ThemeMode,
    pub background: Color,
    pub add: Color,
    pub delete: Color,
    pub add_emphasis: Color,
    pub delete_emphasis: Color,
    pub cursor: Color,
    pub selection: Color,
    pub hunk: Color,
    pub search: Color,
    pub search_current: Color,
    pub ink: Color,
    pub dim: Color,
    pub muted: Color,
    pub code: Color,
    pub accent: Color,
    pub purple: Color,
    pub orange: Color,
    pub success: Color,
    pub danger: Color,
    pub warning: Color,
    pub heading: Color,
}

impl Theme {
    pub const fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Dark => Self::dark(),
            ThemeMode::Light => Self::light(),
        }
    }

    pub const fn dark() -> Self {
        Self {
            mode: ThemeMode::Dark,
            background: Color::Reset,
            add: Color::Rgb(18, 38, 30),
            delete: Color::Rgb(45, 22, 25),
            add_emphasis: Color::Rgb(31, 111, 58),
            delete_emphasis: Color::Rgb(111, 51, 55),
            cursor: Color::Rgb(48, 54, 61),
            selection: Color::Rgb(56, 67, 80),
            hunk: Color::Rgb(27, 31, 36),
            search: Color::Rgb(70, 58, 20),
            search_current: Color::Rgb(140, 106, 18),
            ink: Color::Rgb(1, 4, 9),
            dim: Color::Rgb(125, 133, 144),
            muted: Color::Rgb(140, 149, 159),
            code: Color::Rgb(230, 237, 243),
            accent: Color::Rgb(47, 129, 247),
            purple: Color::Rgb(210, 168, 255),
            orange: Color::Rgb(255, 166, 87),
            success: Color::Rgb(63, 185, 80),
            danger: Color::Rgb(248, 81, 73),
            warning: Color::Rgb(210, 153, 34),
            heading: Color::Rgb(230, 237, 243),
        }
    }

    pub const fn light() -> Self {
        Self {
            mode: ThemeMode::Light,
            background: Color::Reset,
            add: Color::Rgb(218, 251, 225),
            delete: Color::Rgb(255, 235, 233),
            add_emphasis: Color::Rgb(172, 238, 187),
            delete_emphasis: Color::Rgb(255, 206, 203),
            cursor: Color::Rgb(208, 215, 222),
            selection: Color::Rgb(191, 208, 226),
            hunk: Color::Rgb(221, 244, 255),
            search: Color::Rgb(255, 243, 176),
            search_current: Color::Rgb(250, 214, 92),
            ink: Color::Rgb(255, 255, 255),
            dim: Color::Rgb(110, 119, 129),
            muted: Color::Rgb(89, 99, 110),
            code: Color::Rgb(31, 35, 40),
            accent: Color::Rgb(9, 105, 218),
            purple: Color::Rgb(130, 80, 223),
            orange: Color::Rgb(188, 76, 0),
            success: Color::Rgb(26, 127, 55),
            danger: Color::Rgb(207, 34, 46),
            warning: Color::Rgb(154, 103, 0),
            heading: Color::Rgb(31, 35, 40),
        }
    }

    /// Mix a diff background toward the mode-appropriate selection color.
    pub fn cursor_background(self, background: Color) -> Color {
        mix(background, self.cursor, 45)
    }

    pub fn selection_background(self, background: Color) -> Color {
        mix(background, self.selection, 58)
    }
}

fn mix(from: Color, to: Color, percent: u16) -> Color {
    let Color::Rgb(fr, fg, fb) = from else {
        return to;
    };
    let Color::Rgb(tr, tg, tb) = to else {
        return to;
    };

    let channel = |a: u8, b: u8| {
        let mixed = u16::from(a) * (100 - percent) + u16::from(b) * percent;
        (mixed / 100) as u8
    };

    Color::Rgb(channel(fr, tr), channel(fg, tg), channel(fb, tb))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_moves_in_the_right_direction_for_each_mode() {
        assert_eq!(
            Theme::dark().cursor_background(Theme::dark().add),
            Color::Rgb(31, 45, 43)
        );
        assert_eq!(
            Theme::light().cursor_background(Theme::light().add),
            Color::Rgb(213, 234, 223)
        );
    }
}
