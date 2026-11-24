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

    /// Reads a scheme in Ghostty's theme format.
    pub fn parse(text: &str) -> Option<Self> {
        let mut foreground = None;
        let mut background = None;
        let mut palette = [None; 16];

        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };

            match key.trim() {
                "foreground" => foreground = hex(value),
                "background" => background = hex(value),
                "palette" => {
                    let Some((index, color)) = value.split_once('=') else {
                        continue;
                    };
                    let Ok(index) = index.trim().parse::<usize>() else {
                        continue;
                    };
                    if let Some(slot) = palette.get_mut(index) {
                        *slot = hex(color);
                    }
                }
                _ => {}
            }
        }

        let mut colors = [0; 16];
        for (color, parsed) in colors.iter_mut().zip(palette) {
            *color = parsed?;
        }

        Some(Self {
            foreground: foreground?,
            background: background?,
            palette: colors,
        })
    }

    pub const fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Dark => Self::GITHUB_DARK,
            ThemeMode::Light => Self::GITHUB_LIGHT,
        }
    }
}

fn hex(value: &str) -> Option<u32> {
    let digits = value.trim().strip_prefix('#')?;
    if digits.len() != 6 {
        return None;
    }

    u32::from_str_radix(digits, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write;

    #[test]
    fn parses_ghostty_themes() {
        let mut text = String::from(
            "background = #0d1117\nforeground = #e6edf3\ncursor-color = #2f81f7\n",
        );

        for (index, color) in Scheme::GITHUB_DARK.palette.iter().enumerate() {
            let _ = writeln!(text, "palette = {index}=#{color:06x}");
        }

        assert_eq!(Scheme::parse(&text), Some(Scheme::GITHUB_DARK));
        assert_eq!(Scheme::parse("background = #0d1117"), None);
    }
}
