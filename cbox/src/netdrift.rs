//! Reusing a box whose network policy differs from the one requested.
//!
//! `get_or_create` ignores a reused box's options, and BoxLite 0.10.5 does
//! not compare network policy on reuse, so without this a requested
//! `--allow-net` could be silently dropped: a security control that looks
//! applied and is not. The policy can't be changed in place, so the only
//! honest choices are recreate, continue knowingly, or abort.

use std::io::{BufRead, IsTerminal, Write};

use crate::netpolicy::{Policy, Recorded};

/// No network flags means "use whatever the box has": never prompt. Any
/// explicit request prompts unless it matches the recorded policy exactly.
pub fn needs_prompt(requested: &Policy, recorded: &Recorded) -> bool {
    match (requested, recorded) {
        (Policy::Open, _) => false,
        (_, Recorded::Known(p)) => p != requested,
        (_, Recorded::Unknown) => true,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Choice {
    Recreate,
    Continue,
    Abort,
    /// No terminal to ask on; fail closed.
    NoTerminal,
}

pub trait Prompter {
    fn interactive(&self) -> bool;
    /// Show `text`, read one line. `None` on EOF or read error.
    fn ask(&mut self, text: &str) -> Option<String>;
}

pub fn prompt_text(name: &str, recorded: &Recorded, requested: &Policy, running: bool) -> String {
    let recreate = if running {
        "recreate with the new policy (discards the box's disk and state; ends its running sessions)"
    } else {
        "recreate with the new policy (discards the box's disk and state)"
    };
    format!(
        "cbox: box {name} was created with egress: {}\n\
         cbox: you asked for:                  {}\n\
         cbox: its network policy can't be changed without recreating it.\n  \
         [r] {recreate}\n  \
         [c] continue with the box as is\n  \
         [a] abort (default)\n\
         choice [r/c/A]: ",
        recorded.describe(),
        requested.describe(),
    )
}

pub fn choose(p: &mut dyn Prompter, text: &str) -> Choice {
    if !p.interactive() {
        return Choice::NoTerminal;
    }
    match p.ask(text).map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("r" | "recreate") => Choice::Recreate,
        Some("c" | "continue") => Choice::Continue,
        _ => Choice::Abort,
    }
}

/// Real terminal. Blocking: call it from `spawn_blocking`, before `attach`
/// enters raw mode. A single `read_line` always ends on a line or EOF, so
/// it can't cause the uncancellable-stdin shutdown hang `stdin_reader.rs`
/// exists for.
pub struct TtyPrompter;

impl Prompter for TtyPrompter {
    fn interactive(&self) -> bool {
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
    }

    fn ask(&mut self, text: &str) -> Option<String> {
        let mut err = std::io::stderr();
        let _ = err.write_all(text.as_bytes());
        let _ = err.flush();
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netpolicy::resolve;

    struct Fake {
        interactive: bool,
        answer: Option<&'static str>,
        asked: bool,
    }

    impl Prompter for Fake {
        fn interactive(&self) -> bool {
            self.interactive
        }
        fn ask(&mut self, _text: &str) -> Option<String> {
            self.asked = true;
            self.answer.map(String::from)
        }
    }

    fn fake(answer: Option<&'static str>) -> Fake {
        Fake { interactive: true, answer, asked: false }
    }

    fn allow(rules: &[&str]) -> Policy {
        resolve(&rules.iter().map(|s| s.to_string()).collect::<Vec<_>>(), false).unwrap()
    }

    #[test]
    fn no_flags_never_prompts() {
        for rec in [
            Recorded::Known(Policy::Open),
            Recorded::Known(Policy::Disabled),
            Recorded::Known(allow(&["@npm"])),
            Recorded::Unknown,
        ] {
            assert!(!needs_prompt(&Policy::Open, &rec), "{rec:?}");
        }
    }

    #[test]
    fn equal_policies_in_different_order_do_not_prompt() {
        let a = allow(&["@npm", "10.0.0.0/8"]);
        let b = allow(&["10.0.0.0/8", "REGISTRY.npmjs.org"]);
        assert!(!needs_prompt(&a, &Recorded::Known(b)));
        assert!(!needs_prompt(&Policy::Disabled, &Recorded::Known(Policy::Disabled)));
    }

    #[test]
    fn a_differing_or_unknown_policy_prompts() {
        assert!(needs_prompt(&allow(&["@npm"]), &Recorded::Known(Policy::Open)));
        assert!(needs_prompt(&allow(&["@npm"]), &Recorded::Known(allow(&["@github"]))));
        assert!(needs_prompt(&Policy::Disabled, &Recorded::Known(Policy::Open)));
        assert!(needs_prompt(&allow(&["@npm"]), &Recorded::Unknown));
    }

    #[test]
    fn r_c_a_map_to_choices() {
        assert_eq!(choose(&mut fake(Some("r")), "t"), Choice::Recreate);
        assert_eq!(choose(&mut fake(Some("c")), "t"), Choice::Continue);
        assert_eq!(choose(&mut fake(Some("a")), "t"), Choice::Abort);
    }

    #[test]
    fn answers_are_trimmed_and_case_insensitive() {
        assert_eq!(choose(&mut fake(Some(" R\n")), "t"), Choice::Recreate);
        assert_eq!(choose(&mut fake(Some("Recreate\n")), "t"), Choice::Recreate);
        assert_eq!(choose(&mut fake(Some("Continue")), "t"), Choice::Continue);
    }

    #[test]
    fn garbage_aborts() {
        for a in ["", "\n", "yes", "rr", "x"] {
            assert_eq!(choose(&mut fake(Some(a)), "t"), Choice::Abort, "{a:?}");
        }
        assert_eq!(choose(&mut fake(None), "t"), Choice::Abort, "EOF");
    }

    #[test]
    fn no_terminal_never_asks() {
        let mut p = Fake { interactive: false, answer: Some("r"), asked: false };
        assert_eq!(choose(&mut p, "t"), Choice::NoTerminal);
        assert!(!p.asked);
    }

    #[test]
    fn the_prompt_names_both_policies_and_the_choices() {
        let t = prompt_text("demo", &Recorded::Known(Policy::Open), &allow(&["@npm"]), false);
        assert!(t.contains("box demo was created with egress: open"), "{t}");
        assert!(t.contains("registry.npmjs.org"), "{t}");
        assert!(t.contains("[r] recreate"), "{t}");
        assert!(t.contains("[c] continue with the box as is"), "{t}");
        assert!(t.contains("[a] abort (default)"), "{t}");
        assert!(!t.contains("ends its running sessions"), "{t}");

        let t = prompt_text("demo", &Recorded::Known(Policy::Open), &allow(&["@npm"]), true);
        assert!(t.contains("ends its running sessions"), "{t}");
    }
}
