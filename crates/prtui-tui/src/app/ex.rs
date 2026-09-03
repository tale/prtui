use super::action::{Action, Motion};
use super::command::{self, Count};

/// The short spellings Vim trained everyone to type. Anything else is looked
/// up in the command table under its own name, so `:next-comment` works
/// without an entry here.
const ALIASES: &[(&str, &str)] = &[
    ("q", "quit"),
    ("q!", "quit"),
    ("qa", "quit"),
    ("qa!", "quit"),
    ("w", "submit"),
    ("write", "submit"),
    ("h", "help"),
    ("o", "open"),
    ("y", "yank"),
    ("noh", "clear-find"),
    ("nohlsearch", "clear-find"),
];

/// Resolves a `:` line into the action it names.
///
/// A bare number is a line to jump to. Anything else is a command, which is
/// the same vocabulary the keys are bound to.
pub fn parse(line: &str) -> Result<Option<Action>, String> {
    let text = line.trim();
    if text.is_empty() {
        return Ok(None);
    }

    if text == "$" {
        return Ok(Some(Action::Move(Motion::Bottom)));
    }

    if text.chars().all(|character| character.is_ascii_digit()) {
        let number = text
            .parse()
            .map_err(|_| format!("line out of range: {text}"))?;
        return Ok(Some(Action::Move(Motion::Line(number))));
    }

    let name = ALIASES
        .iter()
        .find(|(alias, _)| *alias == text)
        .map_or(text, |(_, name)| name);

    let Some(command) = command::find(name) else {
        return Err(format!("not a command: {text}"));
    };

    Ok(Some((command.build)(Count::default())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(line: &str) -> Result<Option<Action>, String> {
        parse(line)
    }

    #[test]
    fn a_bare_number_is_a_line() {
        assert_eq!(run("42"), Ok(Some(Action::Move(Motion::Line(42)))));
        assert_eq!(run("  7  "), Ok(Some(Action::Move(Motion::Line(7)))));
        assert_eq!(run("$"), Ok(Some(Action::Move(Motion::Bottom))));
        assert_eq!(run(""), Ok(None));
    }

    #[test]
    fn a_name_is_looked_up_in_the_command_table() {
        assert_eq!(run("q"), Ok(Some(Action::Quit)));
        assert_eq!(run("w"), Ok(Some(Action::StartSubmit)));
        assert_eq!(run("submit"), Ok(Some(Action::StartSubmit)));
        assert_eq!(run("next-comment"), Ok(Some(Action::NextComment(1))));
    }

    #[test]
    fn an_unknown_command_says_so() {
        assert_eq!(run("nope"), Err("not a command: nope".to_owned()));
    }

    /// A count typed at the line has to survive the parse rather than
    /// overflowing into a wrong jump.
    #[test]
    fn an_unrepresentable_line_is_refused() {
        let huge = "9".repeat(40);
        assert!(run(&huge).is_err());
    }
}
