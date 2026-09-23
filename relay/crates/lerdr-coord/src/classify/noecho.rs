//! `internal/noecho` — recognizes terminal prompts that read a secret with
//! echo disabled: sudo, ssh, gpg and friends. The phone needs the signal
//! because such a prompt must never reach the draft store, the activity
//! journal or the audit payload hash, and because the generic composer stays
//! locked on a pane the attention classifier cannot read.
//!
//! Matching happens on the content the client will render (the classified,
//! capped frame body) — `Match` answers `no_echo` plus the normalized prompt
//! line for `no_echo_prompt`.

/// `noecho.MaxPromptChars` — bounds both the accepted and the reported
/// prompt line. A real noecho prompt is short; anything longer is prose
/// that merely mentions a password.
pub(crate) const MAX_PROMPT_CHARS: usize = 120;

/// `history.NormalizeLine` — `ansiRe` strip, `\r` removal, right-trim of
/// `" \t\n"`. The history package's matcher is narrower than the parser's
/// `strip_ansi` (CSI `[0-9;]` params only, OSC-BEL, `ESC ()`/`[>=<]`, and
/// the `?`-prefixed `hlJKHfG` set) — kept byte-faithful here.
fn normalize_line(line: &str) -> String {
    let stripped = strip_history_ansi(line);
    stripped
        .replace('\r', "")
        .trim_end_matches([' ', '\t', '\n'])
        .to_owned()
}

/// `ansiRe` = `\x1b\[[0-9;]*[a-zA-Z]|\x1b\][^\x07]*\x07|\x1b[()][0-9A-B]|
/// \x1b[>=<]|\x1b\[\??[0-9;]*[hlJKHfG]` — alternation order matters: the
/// first CSI arm wins for plain-param sequences, the last covers the
/// `?`-prefixed private modes the first cannot reach.
fn strip_history_ansi(line: &str) -> String {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut start = 0usize;
    let mut i = 0usize;
    while i < len {
        if bytes[i] != 0x1b || i + 1 >= len {
            i += 1;
            continue;
        }
        let end = match bytes[i + 1] {
            b'[' => {
                // Arm 1: `[0-9;]*` then a letter.
                let mut j = i + 2;
                while j < len && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                    j += 1;
                }
                if j < len && bytes[j].is_ascii_alphabetic() {
                    Some(j + 1)
                } else {
                    // Arm 5: `\??` then `[0-9;]*` then `[hlJKHfG]`.
                    let mut j = i + 2;
                    if j < len && bytes[j] == b'?' {
                        j += 1;
                    }
                    while j < len && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                        j += 1;
                    }
                    if j < len && matches!(bytes[j], b'h' | b'l' | b'J' | b'K' | b'H' | b'f' | b'G')
                    {
                        Some(j + 1)
                    } else {
                        None
                    }
                }
            }
            // Arm 2: OSC `]` … BEL.
            b']' => bytes[i + 2..]
                .iter()
                .position(|&b| b == 0x07)
                .map(|pos| i + 2 + pos + 1),
            // Arm 3: `ESC (` or `ESC )` then `[0-9A-B]`.
            b'(' | b')'
                if i + 2 < len
                    && (bytes[i + 2].is_ascii_digit()
                        || bytes[i + 2] == b'A'
                        || bytes[i + 2] == b'B') =>
            {
                Some(i + 3)
            }
            // Arm 4: `ESC` then `[>=<]`.
            b'>' | b'=' | b'<' => Some(i + 2),
            _ => None,
        };
        match end {
            Some(end) => {
                out.push_str(&line[start..i]);
                i = end;
                start = end;
            }
            None => i += 1,
        }
    }
    out.push_str(&line[start..]);
    out
}

/// `noecho.lastNonEmptyLine` — the last `NormalizeLine`-trimmed non-empty
/// line, scanning backwards.
fn last_non_empty_line(content: &str) -> Option<String> {
    let mut rest = content;
    while !rest.is_empty() {
        let index = rest.rfind('\n');
        let line = normalize_line(&rest[index.map_or(0, |i| i + 1)..]);
        let line = line.trim();
        if !line.is_empty() {
            return Some(line.to_owned());
        }
        let index = index?;
        rest = &rest[..index];
    }
    None
}

fn starts_fold(haystack: &str, needle: &str) -> bool {
    haystack
        .get(..needle.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(needle))
}

/// `[^\s:]{1,64}` then `\s*:` — the sudo user segment; `None` when the run
/// is empty, over-long, or never terminated by `:`.
fn sudo_user_tail(rest: &str) -> bool {
    let run: usize = rest
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != ':')
        .count();
    if run == 0 || run > 64 {
        return false;
    }
    rest.chars()
        .skip(run)
        .skip_while(|c| c.is_whitespace())
        .collect::<String>()
        == ":"
}

/// `(?i)^\[sudo\] password for [^\s:]{1,64}\s*:$`.
fn sudo_prompt(line: &str) -> bool {
    starts_fold(line, "[sudo] password for ")
        && sudo_user_tail(&line["[sudo] password for ".len()..])
}

/// `(?i)^\S+@\S+'s password\s*:$` — `user@host's password:`.
fn ssh_password_prompt(line: &str) -> bool {
    let lower = line.to_lowercase();
    let Some(at) = lower.find('@') else {
        return false;
    };
    let (user, rest) = lower.split_at(at);
    if user.is_empty() || user.chars().any(char::is_whitespace) {
        return false;
    }
    let Some(suffix) = rest[1..].find("'s password") else {
        return false;
    };
    let host = &rest[1..1 + suffix];
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return false;
    }
    rest[1 + suffix + "'s password".len()..].trim_end().eq(":")
}

/// `\s*:$` — trailing whitespace then a literal `:` at end of line.
fn colon_tail(rest: &str) -> bool {
    rest.trim_end() == ":"
}

/// `[^:]{1,80}:` — a bounded non-colon segment terminated by `:`.
fn bounded_noncolon(rest: &str, max: usize) -> bool {
    let Some(colon) = rest.rfind(':') else {
        return false;
    };
    let segment = &rest[..colon];
    colon == rest.len() - 1
        && (1..=max).contains(&segment.chars().count())
        && !segment.contains(':')
}

/// The oracle's `promptPatterns` — case-insensitive, anchored at both ends.
fn prompt_match(line: &str) -> bool {
    if sudo_prompt(line)
        || ssh_password_prompt(line)
        || starts_fold(line, "password") && colon_tail(&line["password".len()..])
        || starts_fold(line, "password for ")
            && bounded_noncolon(&line["password for ".len()..], 80)
        || starts_fold(line, "enter passphrase") && colon_tail(&line["enter passphrase".len()..])
    {
        return true;
    }
    // `enter passphrase for key '…'\s*:$`
    if starts_fold(line, "enter passphrase for key '") {
        let rest = &line["enter passphrase for key '".len()..];
        if let Some(close) = rest.find('\'') {
            let name = &rest[..close];
            let tail = &rest[close + 1..];
            if !name.is_empty() && name.chars().count() <= 96 && colon_tail(tail) {
                return true;
            }
        }
    }
    // `enter pin(?: for [^:]{1,80})?\s*:$`
    if starts_fold(line, "enter pin") {
        let rest = &line["enter pin".len()..];
        if colon_tail(rest) {
            return true;
        }
        // ` for ` is part of the anchored group — extra whitespace between
        // `pin` and `for` falls through to `\s*:` and fails there.
        if starts_fold(rest, " for ") && bounded_noncolon(&rest[5..], 80) {
            return true;
        }
    }
    // `(?:repeat|verify|confirm) password\s*:$`
    for verb in ["repeat", "verify", "confirm"] {
        if starts_fold(line, verb) {
            let rest = line[verb.len()..].trim_start();
            if starts_fold(rest, "password") && colon_tail(&rest["password".len()..]) {
                return true;
            }
        }
    }
    false
}

/// `rejectPattern` — `(?i)y\s*/\s*n|password policy|password manager` — a
/// yes/no affordance or policy/manager prose reads as a keystroke question,
/// not a secret prompt.
fn rejected(line: &str) -> bool {
    let lower = line.to_lowercase();
    if lower.contains("password policy") || lower.contains("password manager") {
        return true;
    }
    let bytes = lower.as_bytes();
    for (index, &b) in bytes.iter().enumerate() {
        if b != b'y' {
            continue;
        }
        let mut j = index + 1;
        while j < bytes.len() && (bytes[j] as char).is_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'/' {
            continue;
        }
        j += 1;
        while j < bytes.len() && (bytes[j] as char).is_whitespace() {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'n' {
            return true;
        }
    }
    false
}

/// `noecho.Match` — a recognized no-echo secret prompt at the tail of pane
/// content; the returned line is trimmed (and inherently ≤120 runes).
pub(crate) fn match_prompt(content: &str) -> Option<String> {
    let line = last_non_empty_line(content)?;
    if line.chars().count() > MAX_PROMPT_CHARS
        || line.ends_with('?')
        || rejected(&line)
        || !prompt_match(&line)
    {
        return None;
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognized_prompts() {
        for (content, want) in [
            (
                "$ sudo ls\n[sudo] password for alice:",
                "[sudo] password for alice:",
            ),
            (
                "Last login\nalice@host's password:",
                "alice@host's password:",
            ),
            ("output\npassword:", "password:"),
            ("output\nPassword for root:", "Password for root:"),
            ("gpg\nenter passphrase:", "enter passphrase:"),
            (
                "ssh\nEnter passphrase for key '/home/a/.ssh/id_ed25519':",
                "Enter passphrase for key '/home/a/.ssh/id_ed25519':",
            ),
            ("vault\nenter PIN:", "enter PIN:"),
            ("setup\nRepeat password:", "Repeat password:"),
            ("setup\nconfirm password:", "confirm password:"),
        ] {
            assert_eq!(match_prompt(content).as_deref(), Some(want), "{content:?}");
        }
    }

    #[test]
    fn rejects_prose_and_questions() {
        for content in [
            "continue? (y/n) password:",
            "password: continue? (y/n)",
            "the password policy requires 12 chars",
            "open your password manager:",
            "change your password?:",
            "",
            "   \n  \n",
            "$ ls\n",
        ] {
            assert_eq!(match_prompt(content), None, "{content:?}");
        }
        // Over-long lines are prose, not prompts.
        let long = format!("password for {}", "x".repeat(120));
        assert_eq!(match_prompt(&long), None);
    }

    #[test]
    fn ansi_and_carriage_returns_normalize() {
        assert_eq!(
            match_prompt("$ sudo ls\r\n\x1b[32m[sudo] password for alice:\x1b[0m\r\n").as_deref(),
            Some("[sudo] password for alice:")
        );
        // The tail scan skips trailing blank/ansi-only lines.
        assert_eq!(
            match_prompt("out\npassword:\n\x1b[K  \n").as_deref(),
            Some("password:")
        );
    }
}
