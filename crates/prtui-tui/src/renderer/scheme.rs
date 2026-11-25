// Base16 schemes from schemes.conf, keyed by slug.
pub(super) const SCHEMES: &[(&str, &str)] =
    include!(concat!(env!("OUT_DIR"), "/schemes.rs"));

/// A Base16 scheme: sixteen slots from `base00` to `base0F`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scheme {
    pub base: [u32; 16],
}

impl Scheme {
    /// Finds a bundled scheme by slug, ignoring case.
    pub fn named(slug: &str) -> Option<Self> {
        SCHEMES
            .iter()
            .find(|(known, _)| known.eq_ignore_ascii_case(slug))
            .and_then(|(_, text)| Self::parse(text))
    }

    /// Reads the palette out of a Base16 scheme file.
    pub fn parse(text: &str) -> Option<Self> {
        let mut base = [None; 16];

        for line in text.lines() {
            let Some((key, value)) = line.trim().split_once(':') else {
                continue;
            };
            let Some(slot) = key.strip_prefix("base0") else {
                continue;
            };
            let Ok(index) = usize::from_str_radix(slot, 16) else {
                continue;
            };

            base[index] = hex(value);
        }

        let mut slots = [0; 16];
        for (slot, parsed) in slots.iter_mut().zip(base) {
            *slot = parsed?;
        }

        Some(Self { base: slots })
    }

    pub const fn background(&self) -> u32 {
        self.base[0x0]
    }

    pub const fn foreground(&self) -> u32 {
        self.base[0x5]
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
        let named = |slug| Scheme::named(slug).expect("GitHub is bundled");
        Self {
            light: named("github"),
            dark: named("github-dark"),
        }
    }

    /// Reads `auto`, one scheme, or Ghostty's `light:NAME,dark:NAME`.
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

// Values look like `"#191724"`, sometimes with a trailing comment.
fn hex(value: &str) -> Option<u32> {
    let digits = value.trim().trim_start_matches('"').strip_prefix('#')?;
    u32::from_str_radix(digits.get(..6)?, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_bundled_scheme() {
        for (name, text) in SCHEMES {
            assert!(Scheme::parse(text).is_some(), "{name} does not parse");
        }

        let github = Scheme::named("GitHub-Dark").unwrap();
        assert_eq!(github.background(), 0x0d1117);
        assert_eq!(github.base[0x8], 0xff7b72);
        assert_eq!(Scheme::parse("base00: \"#0d1117\""), None);
    }

    #[test]
    fn parses_theme_pairs() {
        let mocha = Scheme::named("catppuccin-mocha").unwrap();
        let latte = Scheme::named("catppuccin-latte").unwrap();

        let pair =
            Schemes::parse("dark:Catppuccin-Mocha, light:catppuccin-latte");
        assert_eq!(
            pair,
            Some(Schemes {
                light: latte,
                dark: mocha
            })
        );

        let single = Schemes::parse("catppuccin-mocha");
        assert_eq!(
            single,
            Some(Schemes {
                light: mocha,
                dark: mocha
            })
        );

        assert_eq!(Schemes::parse("auto"), Some(Schemes::github()));
        assert_eq!(Schemes::parse("light:catppuccin-latte,dark:nope"), None);
    }
}
