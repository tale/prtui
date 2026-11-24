use super::Scheme;
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
    pub cyan: Color,
    pub success: Color,
    pub danger: Color,
    pub warning: Color,
    pub heading: Color,
}

const RED: usize = 1;
const GREEN: usize = 2;
const YELLOW: usize = 3;
const BLUE: usize = 4;
const MAGENTA: usize = 5;
const CYAN: usize = 6;

const LUMA_MAX: u32 = 255_000;

impl Theme {
    pub fn for_mode(mode: ThemeMode) -> Self {
        Self::from_scheme(&Scheme::for_mode(mode))
    }

    pub fn dark() -> Self {
        Self::from_scheme(&Scheme::GITHUB_DARK)
    }

    pub fn light() -> Self {
        Self::from_scheme(&Scheme::GITHUB_LIGHT)
    }

    pub fn from_scheme(scheme: &Scheme) -> Self {
        let foreground = rgb(scheme.foreground);
        let background = rgb(scheme.background);
        let ansi = |index: usize| rgb(scheme.palette[index]);

        let luma = luma(scheme.background);
        let mode = if luma >= LUMA_MAX / 2 {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };

        let strength = |dark: u16, light: u16| {
            let mixed =
                u32::from(dark) * (LUMA_MAX - luma) + u32::from(light) * luma;
            (mixed / LUMA_MAX) as u16
        };
        let surface =
            |hue, dark, light| mix(background, hue, strength(dark, light));
        let text =
            |dark, light| mix(foreground, background, strength(dark, light));

        Self {
            mode,
            background: Color::Reset,
            add: surface(ansi(GREEN), 12, 18),
            delete: surface(ansi(RED), 13, 12),
            add_emphasis: surface(ansi(GREEN), 50, 40),
            delete_emphasis: surface(ansi(RED), 45, 28),
            cursor: surface(foreground, 18, 20),
            selection: surface(ansi(BLUE), 28, 32),
            hunk: surface(ansi(BLUE), 10, 12),
            search: surface(ansi(YELLOW), 25, 30),
            search_current: surface(ansi(YELLOW), 55, 60),
            ink: background,
            dim: text(48, 36),
            muted: text(40, 26),
            code: foreground,
            accent: ansi(BLUE),
            purple: ansi(MAGENTA),
            cyan: ansi(CYAN),
            success: ansi(GREEN),
            danger: ansi(RED),
            warning: ansi(YELLOW),
            heading: foreground,
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

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

const fn luma(hex: u32) -> u32 {
    (hex >> 16 & 0xff) * 299 + (hex >> 8 & 0xff) * 587 + (hex & 0xff) * 114
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
