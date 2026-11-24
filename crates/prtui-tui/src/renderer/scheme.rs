use super::ThemeMode;

/// A terminal color scheme in the shape iTerm2-Color-Schemes publishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scheme {
    pub foreground: u32,
    pub background: u32,
    pub palette: [u32; 16],
}

impl Scheme {
    pub const GITHUB_DARK: Self = Self {
        foreground: 0xe6edf3,
        background: 0x0d1117,
        palette: [
            0x484f58, 0xff7b72, 0x3fb950, 0xd29922, 0x58a6ff, 0xbc8cff,
            0x39c5cf, 0xb1bac4, 0x6e7681, 0xffa198, 0x56d364, 0xe3b341,
            0x79c0ff, 0xd2a8ff, 0x56d4dd, 0xffffff,
        ],
    };

    pub const GITHUB_LIGHT: Self = Self {
        foreground: 0x1f2328,
        background: 0xffffff,
        palette: [
            0x24292f, 0xcf222e, 0x116329, 0x4d2d00, 0x0969da, 0x8250df,
            0x1b7c83, 0x6e7781, 0x57606a, 0xa40e26, 0x1a7f37, 0x633c01,
            0x218bff, 0xa475f9, 0x3192aa, 0x8c959f,
        ],
    };

    pub const fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Dark => Self::GITHUB_DARK,
            ThemeMode::Light => Self::GITHUB_LIGHT,
        }
    }
}
