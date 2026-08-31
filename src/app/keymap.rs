use super::action::Action;
use super::command::{self, Command, Count};
use super::keys::{self, Key};
use super::mode::Mode;
use termina::event::KeyEvent;

const MAX_COUNT: usize = 999_999;

/// The built-in keymap.
///
/// A row is the modes it is live in, the chord that fires it, and the command
/// it names. Modes are the letters `Mode::from_letter` reads and a chord is
/// Vim's own notation, so a user keymap parses into exactly these bindings
/// rather than into a second mechanism.
const DEFAULT: &[(&str, &str, &str)] = &[
    ("nv", "j", "move-down"),
    ("nv", "k", "move-up"),
    ("nv", "<Down>", "move-down"),
    ("nv", "<Up>", "move-up"),
    ("nv", "<C-d>", "half-page-down"),
    ("nv", "<C-u>", "half-page-up"),
    ("nv", "gg", "goto-first-line"),
    ("nv", "G", "goto-last-line"),
    ("nv", "]", "next-file"),
    ("nv", "[", "prev-file"),
    ("n", "n", "next-match"),
    ("n", "N", "prev-match"),
    ("n", "}", "next-comment"),
    ("n", "{", "prev-comment"),
    ("nv", "<Tab>", "toggle-pane"),
    ("nv", "f", "toggle-tree"),
    ("n", "h", "focus-files"),
    ("n", "l", "focus-diff"),
    ("n", "<Left>", "focus-files"),
    ("n", "<Right>", "focus-diff"),
    ("n", "/", "find"),
    ("n", ":", "command-line"),
    ("n", "?", "help"),
    ("n", "o", "overview"),
    ("n", "gx", "open"),
    ("nv", "y", "yank"),
    ("n", "v", "enter-visual"),
    ("n", "V", "enter-visual"),
    ("v", "v", "leave-visual"),
    ("v", "V", "leave-visual"),
    ("nv", "c", "comment"),
    ("n", "C", "file-comment"),
    ("n", "e", "edit-draft"),
    ("n", "d", "delete-draft"),
    ("n", "R", "toggle-resolved"),
    ("n", "x", "toggle-viewed"),
    ("n", "s", "submit"),
    ("n", "zk", "expand-up"),
    ("n", "zj", "expand-down"),
    ("n", "za", "expand-all"),
    ("n", "zR", "expand-file"),
    ("f", "<Down>", "move-down"),
    ("f", "<Up>", "move-up"),
    ("s", "<Down>", "next-match"),
    ("s", "<Up>", "prev-match"),
    ("c", "<Up>", "history-prev"),
    ("c", "<Down>", "history-next"),
    // Every prompt recalls with readline's chord. The arrows are already
    // spoken for in two of the three, stepping files and hits.
    ("fsc", "<C-p>", "history-prev"),
    ("fsc", "<C-n>", "history-next"),
    // Readline's own editing, in every prompt there is. A prompt is a line of
    // text in a terminal, and these are the chords a terminal has trained
    // everyone to reach for on one.
    ("ifscr", "<C-a>", "line-start"),
    ("ifscr", "<C-e>", "line-end"),
    ("ifscr", "<C-b>", "char-left"),
    ("ifscr", "<C-f>", "char-right"),
    ("ifscr", "<A-b>", "word-left"),
    ("ifscr", "<A-f>", "word-right"),
    ("ifscr", "<C-d>", "delete-char"),
    ("ifscr", "<A-BS>", "delete-word-left"),
    ("ifscr", "<C-w>", "delete-to-blank"),
    ("ifscr", "<A-d>", "delete-word-right"),
    ("ifscr", "<C-u>", "delete-to-start"),
    ("ifscr", "<C-k>", "delete-to-end"),
    ("r", "<Tab>", "next-verdict"),
    ("r", "<S-Tab>", "prev-verdict"),
    ("n", "<CR>", "activate"),
    ("i", "<CR>", "commit-comment"),
    ("f", "<CR>", "accept-filter"),
    ("s", "<CR>", "accept-search"),
    ("c", "<CR>", "run-command-line"),
    ("r", "<CR>", "commit-submit"),
    ("n", "<Esc>", "escape"),
    ("v", "<Esc>", "leave-visual"),
    ("i", "<Esc>", "cancel-comment"),
    ("f", "<Esc>", "cancel-filter"),
    ("s", "<Esc>", "cancel-search"),
    ("c", "<Esc>", "cancel-command-line"),
    ("r", "<Esc>", "cancel-submit"),
    ("nvifscrho", "<C-c>", "quit"),
    ("nv", "q", "quit"),
    ("ho", "j", "move-down"),
    ("ho", "k", "move-up"),
    ("ho", "<Down>", "move-down"),
    ("ho", "<Up>", "move-up"),
    ("ho", "<C-d>", "half-page-down"),
    ("ho", "<C-u>", "half-page-up"),
    ("ho", "gg", "goto-first-line"),
    ("ho", "G", "goto-last-line"),
    ("ho", "/", "find"),
    ("ho", "n", "next-match"),
    ("ho", "N", "prev-match"),
    ("ho", "<Esc>", "close-panel"),
    ("ho", "q", "close-panel"),
    ("h", "?", "close-panel"),
    ("o", "o", "close-panel"),
];

/// One line of the key reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reference {
    Heading(&'static str),
    Entry {
        /// The chords bound to the command, or empty when only `:` reaches it.
        keys: String,
        name: &'static str,
        summary: &'static str,
    },
}

/// The result of feeding one key into the command parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Action(Action),
    /// The key is a valid prefix, but the command needs more input.
    Pending,
    /// The key does not belong to the application keymap in this mode.
    Unbound,
}

/// The modes a binding is live in, one bit each. Wide enough for every mode
/// there is: a mask that cannot hold them all shifts off its own end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Modes(u16);

impl Modes {
    fn parse(letters: &str) -> Option<Self> {
        let mut mask = 0;

        for letter in letters.chars() {
            mask |= 1 << Mode::from_letter(letter)? as u16;
        }

        Some(Self(mask))
    }

    const fn contains(self, mode: Mode) -> bool {
        self.0 & (1 << mode as u16) != 0
    }
}

/// One chord and what it runs.
struct Binding {
    modes: Modes,
    chord: Vec<Key>,
    command: &'static Command,
}

/// The bindings, plus the half-typed parts of a command: a count prefix (`12j`)
/// and the keys of a chord still waiting for its last one (`g`, `z`).
pub struct Keymap {
    bindings: Vec<Binding>,
    count: Option<usize>,
    pending: Vec<Key>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::from_table(DEFAULT)
    }
}

impl Keymap {
    /// Builds a keymap from mode/chord/command rows. Rows that name a mode,
    /// notation or command that does not exist are dropped, which is what lets
    /// one bad line in a user keymap cost only its own binding.
    pub fn from_table(table: &[(&str, &str, &str)]) -> Self {
        let bindings = table
            .iter()
            .filter_map(|&(modes, chord, name)| {
                Some(Binding {
                    modes: Modes::parse(modes)?,
                    chord: keys::chord(chord)?,
                    command: command::find(name)?,
                })
            })
            .collect();

        Self {
            bindings,
            count: None,
            pending: Vec::new(),
        }
    }

    /// The key reference: the command table, annotated with the chords bound
    /// to each command.
    ///
    /// Reading it off the bindings rather than off a second list is what stops
    /// the two disagreeing. A command with no chord still appears, since `:` is
    /// how it is reached.
    pub fn reference(&self) -> Vec<Reference> {
        let mut lines = Vec::new();
        let mut group = "";

        for command in command::COMMANDS {
            if command.group != group {
                group = command.group;
                lines.push(Reference::Heading(group));
            }

            // One command answers to the same chord in several modes, and
            // reading `j` three times says nothing the first one did not.
            let mut keys: Vec<String> = Vec::new();
            for binding in self
                .bindings
                .iter()
                .filter(|binding| binding.command.name == command.name)
            {
                let chord = keys::render(&binding.chord);
                if !keys.contains(&chord) {
                    keys.push(chord);
                }
            }

            lines.push(Reference::Entry {
                keys: keys.join("  "),
                name: command.name,
                summary: command.summary,
            });
        }

        lines
    }

    pub fn pending_hint(&self) -> String {
        let count = self
            .count
            .map(|count| count.to_string())
            .unwrap_or_default();

        format!("{count}{}", keys::render(&self.pending))
    }

    pub fn clear(&mut self) {
        self.count = None;
        self.pending.clear();
    }

    /// The mode is the table's own addressing, not a fact about the app: a
    /// binding is looked up by it and nothing is handed it afterwards.
    pub fn resolve(&mut self, mode: Mode, event: KeyEvent) -> Resolution {
        let key = Key::from_event(event);

        // A digit is a count unless a chord being typed wants it as its next
        // key, which is what keeps `12j` and a hypothetical `g4` from fighting.
        if mode.takes_count()
            && self.is_count_digit(key)
            && !self.extends_chord(mode, key)
        {
            self.add_digit(key);
            return Resolution::Pending;
        }

        self.pending.push(key);

        if let Some(command) = self.exact(mode) {
            let count = Count::new(self.count);
            self.clear();
            return Resolution::Action((command.build)(count));
        }

        if self.has_longer(mode) {
            return Resolution::Pending;
        }

        self.clear();
        Resolution::Unbound
    }

    /// A leading zero is not a count prefix in a Vim-style keymap.
    fn is_count_digit(&self, key: Key) -> bool {
        key.as_char().is_some_and(|character| {
            character.is_ascii_digit()
                && !(character == '0' && self.count.is_none())
        })
    }

    fn add_digit(&mut self, key: Key) {
        let digit = key
            .as_char()
            .and_then(|character| character.to_digit(10))
            .unwrap_or(0) as usize;

        self.count = Some(
            self.count
                .unwrap_or(0)
                .saturating_mul(10)
                .saturating_add(digit)
                .min(MAX_COUNT),
        );
    }

    fn live(&self, mode: Mode) -> impl Iterator<Item = &Binding> {
        self.bindings
            .iter()
            .filter(move |binding| binding.modes.contains(mode))
    }

    fn exact(&self, mode: Mode) -> Option<&'static Command> {
        self.live(mode)
            .find(|binding| binding.chord == self.pending)
            .map(|binding| binding.command)
    }

    fn has_longer(&self, mode: Mode) -> bool {
        self.live(mode).any(|binding| {
            binding.chord.len() > self.pending.len()
                && binding.chord.starts_with(&self.pending)
        })
    }

    fn extends_chord(&self, mode: Mode, key: Key) -> bool {
        if self.pending.is_empty() {
            return self
                .live(mode)
                .any(|binding| binding.chord.first() == Some(&key));
        }

        let mut probe = self.pending.clone();
        probe.push(key);

        self.live(mode)
            .any(|binding| binding.chord.starts_with(&probe))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dropped row is silent, so the built-in table is checked rather than
    /// trusted.
    #[test]
    fn every_builtin_binding_parses() {
        assert_eq!(Keymap::default().bindings.len(), DEFAULT.len());
    }

    /// Two bindings for the same chord in the same mode means the second one
    /// never fires.
    #[test]
    fn no_chord_is_bound_twice_in_one_mode() {
        let keymap = Keymap::default();

        for binding in &keymap.bindings {
            for other in &keymap.bindings {
                let is_same = std::ptr::eq(binding, other);
                let overlaps = binding.modes.0 & other.modes.0 != 0;
                assert!(
                    is_same || !overlaps || binding.chord != other.chord,
                    "{} is bound twice",
                    keys::render(&binding.chord)
                );
            }
        }
    }

    /// A chord's prefix must not also be a command, or the shorter one wins
    /// and the longer can never be typed.
    #[test]
    fn no_chord_shadows_a_longer_one() {
        let keymap = Keymap::default();

        for binding in keymap.bindings.iter().filter(|b| b.chord.len() > 1) {
            let prefix = &binding.chord[..1];
            assert!(
                !keymap.bindings.iter().any(|other| {
                    other.chord == prefix
                        && other.modes.0 & binding.modes.0 != 0
                }),
                "{} is shadowed by its own prefix",
                keys::render(&binding.chord)
            );
        }
    }
}
