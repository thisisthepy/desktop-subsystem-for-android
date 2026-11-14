//! Arguments accepted by the guest's su client.

/// What an app asked su for.
#[derive(Debug, PartialEq, Eq)]
pub enum SuCommand {
    /// Run a shell, taking commands from standard input.
    Shell,
    /// Run a command through the shell.
    Command(String),
    /// Print su's version and end, without asking for root. Root apps call `su -v` or `su -V`
    /// to find out whether su is there at all.
    Version {
        /// `-V`: the version number alone.
        code: bool,
    },
}

/// The version su prints for `su -v`.
pub const VERSION: &str = "DSA:SU";

/// The version number su prints for `su -V`.
pub const VERSION_CODE: u32 = 1;

const USAGE: &str = "usage: su [options] [root|0] [command...]\n\
                     options: -c <command>, --command <command>, -, -l, --login, -m, -p, \
                     --preserve-environment, -mm, -M, --mount-master, -s <shell>, \
                     --shell <shell>, -v, -V";

/// Parses the su forms root apps commonly use, without interpreting shell syntax:
///
/// - `su`, `su -`, `su root`, `su 0`: a shell, reading its commands from standard input.
/// - `su -c <command> [words...]`, `su --command <command>`, `su --command=<command>`: the
///   command, with every word after it, as the su most root apps are written for takes it.
/// - `su root <command> [words...]` and `su 0 <command> [words...]`: Android's own su form,
///   the command words after the target user.
/// - `-v`, `--version`, `-V`: the version, with nothing asked.
///
/// The target user may come before or after the options. The options that change nothing
/// here (`-`, `-l`, `--login`, `-m`, `-p`, `--preserve-environment`, `-mm`, `-M`,
/// `--mount-master`, `-s <shell>`, `--shell <shell>`, `--shell=<shell>`) are taken and
/// ignored: the shell is always the system shell, in the system's mount namespace. Any other
/// target user, and any other option, is refused, so a call that wants something su cannot
/// give fails rather than running as something else.
pub fn parse(args: &[String]) -> Result<SuCommand, String> {
    let mut saw_user = false;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-c" | "--command" => {
                let rest = &args[i + 1..];
                return command_from(rest);
            }
            "-v" | "--version" => return Ok(SuCommand::Version { code: false }),
            "-V" => return Ok(SuCommand::Version { code: true }),
            "-"
            | "-l"
            | "--login"
            | "-m"
            | "-p"
            | "--preserve-environment"
            | "-mm"
            | "-M"
            | "--mount-master" => i += 1,
            "-s" | "--shell" => {
                if args.get(i + 1).is_none_or(|shell| shell.trim().is_empty()) {
                    return Err(format!("{arg} needs a shell\n{USAGE}"));
                }
                i += 2;
            }
            "root" | "0" if !saw_user => {
                saw_user = true;
                i += 1;
            }
            _ => {
                if let Some(command) = arg.strip_prefix("--command=") {
                    let mut words = vec![command.to_string()];
                    words.extend(args[i + 1..].iter().cloned());
                    return command_from(&words);
                }
                if let Some(shell) = arg.strip_prefix("--shell=") {
                    if shell.trim().is_empty() {
                        return Err(format!("--shell needs a shell\n{USAGE}"));
                    }
                    i += 1;
                    continue;
                }
                if arg.starts_with('-') {
                    return Err(format!("unknown option {arg}\n{USAGE}"));
                }
                if !saw_user {
                    return Err(format!(
                        "only root (uid 0) can be asked for, not {arg:?}\n{USAGE}"
                    ));
                }
                // Android's own form: the words after the target user are the command.
                return command_from(&args[i..]);
            }
        }
    }
    Ok(SuCommand::Shell)
}

fn command_from(words: &[String]) -> Result<SuCommand, String> {
    let command = words.join(" ");
    if command.trim().is_empty() {
        return Err(format!("-c needs a command\n{USAGE}"));
    }
    Ok(SuCommand::Command(command))
}

/// Splits a command string into words as a shell would, for the host's approval prompt and
/// log. The command itself runs as given; this only names it.
pub fn split_command(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut single_quote = false;
    let mut double_quote = false;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        if single_quote {
            if ch == '\'' {
                single_quote = false;
            } else {
                current.push(ch);
            }
            continue;
        }
        if double_quote {
            match ch {
                '"' => double_quote = false,
                '\\' if matches!(chars.peek(), Some('"' | '\\' | '$' | '`')) => {
                    current.extend(chars.next());
                }
                c => current.push(c),
            }
            continue;
        }
        match ch {
            '\'' => {
                single_quote = true;
                in_word = true;
            }
            '"' => {
                double_quote = true;
                in_word = true;
            }
            '\\' => {
                in_word = true;
                current.extend(chars.next());
            }
            c if c.is_ascii_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                current.push(c);
            }
        }
    }
    if in_word {
        words.push(current);
    }
    words
}

/// The commands in the text an app passed su on standard input, one per line, joined with
/// `; ` as a shell runs them in turn, for the host's approval prompt and log.
pub fn stdin_commands(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

/// The words the host's approval prompt and log show for `command`, the text the shell runs:
/// the words of a simple command, as a shell would split them, or, for anything a shell
/// would read as more than one simple command (`;`, `&&`, a pipe, a redirect, a line break,
/// a substitution), the whole text as one script after `sh -c`, so the prompt shows exactly
/// what runs rather than words cut where the shell would not cut them.
pub fn command_words(command: &str) -> Vec<String> {
    let simple = !command.chars().any(|c| {
        matches!(
            c,
            ';' | '&' | '|' | '<' | '>' | '\n' | '\r' | '`' | '$' | '(' | ')'
        )
    });
    if simple {
        return split_command(command);
    }
    let script = stdin_commands(command);
    vec!["sh".into(), "-c".into(), script]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| (*a).to_string()).collect()
    }

    #[test]
    fn fr35_su_accepts_a_shell_and_a_command() {
        assert_eq!(parse(&[]), Ok(SuCommand::Shell));
        assert_eq!(
            parse(&s(&["-c", "id; echo ok"])),
            Ok(SuCommand::Command("id; echo ok".into()))
        );
        assert_eq!(
            parse(&s(&["-c", "echo", "root"])),
            Ok(SuCommand::Command("echo root".into()))
        );
        assert_eq!(
            parse(&s(&["0", "sh", "-c", "id"])),
            Ok(SuCommand::Command("sh -c id".into()))
        );
        assert_eq!(parse(&s(&["-v"])), Ok(SuCommand::Version { code: false }));
        assert_eq!(parse(&s(&["-V"])), Ok(SuCommand::Version { code: true }));
    }

    #[test]
    fn fr35_su_rejects_incomplete_or_unknown_options() {
        for refused in [
            &["-c"][..],
            &["-c", ""],
            &["--command="],
            &["-s"],
            &["--shell="],
            &["--unknown"],
            &["shell"],
            &["1000"],
            &["2000", "-c", "id"],
        ] {
            assert!(parse(&s(refused)).is_err(), "{refused:?}");
        }
    }

    #[test]
    fn fr35_commands_on_standard_input_are_named_one_after_another() {
        assert_eq!(stdin_commands("id\n"), "id");
        assert_eq!(
            stdin_commands("id\n\n  echo ok  \nexit\n"),
            "id; echo ok; exit"
        );
        assert_eq!(stdin_commands("  \n"), "");
    }

    #[test]
    fn fr35_split_command_breaks_words_like_a_shell() {
        assert_eq!(split_command("id"), s(&["id"]));
        assert_eq!(split_command("id -u"), s(&["id", "-u"]));
        assert_eq!(
            split_command("sh -c 'echo hi' \"a \\\"b\\\"\" c\\ d"),
            s(&["sh", "-c", "echo hi", "a \"b\"", "c d"])
        );
    }

    #[test]
    fn fr35_the_prompt_shows_exactly_the_command_that_runs() {
        assert_eq!(command_words("id -u"), s(&["id", "-u"]));
        assert_eq!(
            command_words("id; echo ok"),
            s(&["sh", "-c", "id; echo ok"])
        );
        assert_eq!(
            command_words("id -u\necho done\n"),
            s(&["sh", "-c", "id -u; echo done"])
        );
        assert_eq!(
            command_words("cat /data/x > /sdcard/x"),
            s(&["sh", "-c", "cat /data/x > /sdcard/x"])
        );
    }
}
