// Ghostty themes from schemes.conf, sorted by name.
const SCHEMES: &[(&str, &str)] =
    include!(concat!(env!("OUT_DIR"), "/schemes.rs"));

/// A terminal color scheme in the shape iTerm2-Color-Schemes publishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scheme {
    pub foreground: u32,
    pub background: u32,
    pub palette: [u32; 16],
}

impl Scheme {
    /// Finds a bundled scheme by name, ignoring case.
    pub fn named(name: &str) -> Option<Self> {
        SCHEMES
            .iter()
            .find(|(known, _)| known.eq_ignore_ascii_case(name))
            .and_then(|(_, text)| Self::parse(text))
    }

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
}

/// The schemes to use on light and dark terminal backgrounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schemes {
    pub light: Scheme,
    pub dark: Scheme,
}

impl Schemes {
    pub fn github() -> Self {
        let named = |name| Scheme::named(name).expect("GitHub is bundled");
        Self {
            light: named("GitHub Light Default"),
            dark: named("GitHub Dark Default"),
        }
    }

    /// Reads `auto`, one scheme name, or Ghostty's `light:NAME,dark:NAME`.
    pub fn parse(value: &str) -> Option<Self> {
        if value == "auto" {
            return Some(Self::github());
        }

        if !value.contains(',') {
            let scheme = Scheme::named(value)?;
            return Some(Self {
                light: scheme,
                dark: scheme,
            });
        }

        let (mut light, mut dark) = (None, None);
        for part in value.split(',') {
            match part.trim().split_once(':') {
                Some(("light", name)) => light = Scheme::named(name.trim()),
                Some(("dark", name)) => dark = Scheme::named(name.trim()),
                _ => return None,
            }
        }

        Some(Self {
            light: light?,
            dark: dark?,
        })
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

    #[test]
    fn parses_every_bundled_scheme() {
        for (name, text) in SCHEMES {
            assert!(Scheme::parse(text).is_some(), "{name} does not parse");
        }

        let github = Scheme::named("github dark default").unwrap();
        assert_eq!(github.background, 0x0d1117);
        assert_eq!(github.palette[1], 0xff7b72);
        assert_eq!(Scheme::parse("background = #0d1117"), None);
    }

    #[test]
    fn parses_theme_pairs() {
        let mocha = Scheme::named("Catppuccin Mocha").unwrap();
        let latte = Scheme::named("Catppuccin Latte").unwrap();

        let pair =
            Schemes::parse("dark:catppuccin mocha, light:Catppuccin Latte");
        assert_eq!(
            pair,
            Some(Schemes {
                light: latte,
                dark: mocha
            })
        );

        let single = Schemes::parse("Catppuccin Mocha");
        assert_eq!(
            single,
            Some(Schemes {
                light: mocha,
                dark: mocha
            })
        );

        assert_eq!(Schemes::parse("auto"), Some(Schemes::github()));
        assert_eq!(Schemes::parse("light:Catppuccin Latte,dark:nope"), None);
    }
}
