//! The rewrite popup of the reply box (`Ctrl-R`): choose how the local model rewrites the reply
//! (a custom instruction, grammar, more formal, more fluent, more friendly), wait for it, look at
//! the result and take it or not. Pure state; the loop sends the request and calls
//! [`RewriteForm::finish`] with the answer.

use haste_core::ai::{AiOutcome, RewriteMode, INSTRUCTION_MAX};

use crate::app::Key;

/// The choices, in the order shown.
pub const OPTIONS: [(&str, RewriteMode); 5] = [
    ("Custom command…", RewriteMode::Custom),
    ("Check grammar", RewriteMode::Grammar),
    ("More formal", RewriteMode::Formal),
    ("More fluent", RewriteMode::Fluent),
    ("More friendly", RewriteMode::Friendly),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Picking one of [`OPTIONS`].
    Choose,
    /// Typing the custom instruction.
    Instruction,
    /// The request is out.
    Working,
    /// The model's text, waiting for Enter (take it) or Esc (leave the reply as it is).
    Preview(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteForm {
    pub thread_id: String,
    /// The reply as it was when the popup opened.
    pub draft: String,
    pub phase: Phase,
    pub selected: usize,
    pub instruction: String,
    /// One line under the choices: why nothing happened.
    pub message: Option<String>,
}

/// What a key did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewriteOutcome {
    Open,
    /// Esc: back to the reply as it was.
    Close,
    /// Ask the daemon.
    Start {
        mode: RewriteMode,
        instruction: Option<String>,
    },
    /// Enter on the result: this text becomes the reply.
    Accept(String),
}

impl RewriteForm {
    #[must_use]
    pub fn new(thread_id: &str, draft: &str) -> Self {
        Self {
            thread_id: thread_id.to_owned(),
            draft: draft.to_owned(),
            phase: Phase::Choose,
            selected: 0,
            instruction: String::new(),
            message: None,
        }
    }

    fn choose(&mut self, index: usize) -> RewriteOutcome {
        self.selected = index;
        // nothing to rewrite: say so here and make no call
        if self.draft.trim().is_empty() {
            self.message = Some("the reply is empty: write something first".into());
            return RewriteOutcome::Open;
        }
        self.message = None;
        let mode = OPTIONS[index].1;
        if mode == RewriteMode::Custom {
            self.phase = Phase::Instruction;
            return RewriteOutcome::Open;
        }
        self.phase = Phase::Working;
        RewriteOutcome::Start {
            mode,
            instruction: None,
        }
    }

    pub fn handle(&mut self, key: Key) -> RewriteOutcome {
        match &self.phase {
            Phase::Choose => match key {
                Key::Esc => return RewriteOutcome::Close,
                Key::Down | Key::Tab | Key::Char('j') => {
                    self.selected = (self.selected + 1).min(OPTIONS.len() - 1);
                }
                Key::Up | Key::BackTab | Key::Char('k') => {
                    self.selected = self.selected.saturating_sub(1);
                }
                Key::Enter => return self.choose(self.selected),
                Key::Char(c @ '1'..='5') => {
                    return self.choose(usize::from(c as u8 - b'1'));
                }
                _ => {}
            },
            Phase::Instruction => match key {
                Key::Esc => {
                    self.phase = Phase::Choose;
                    self.message = None;
                }
                Key::Backspace => {
                    self.instruction.pop();
                }
                Key::Enter => {
                    if self.instruction.trim().is_empty() {
                        self.message = Some("type what the model should do".into());
                    } else {
                        self.message = None;
                        self.phase = Phase::Working;
                        return RewriteOutcome::Start {
                            mode: RewriteMode::Custom,
                            instruction: Some(self.instruction.trim().to_owned()),
                        };
                    }
                }
                Key::Char(c)
                    if !c.is_control() && self.instruction.chars().count() < INSTRUCTION_MAX =>
                {
                    self.instruction.push(c);
                }
                _ => {}
            },
            // a request is out: Esc stops waiting (the answer is dropped when it comes)
            Phase::Working => {
                if key == Key::Esc {
                    return RewriteOutcome::Close;
                }
            }
            Phase::Preview(text) => match key {
                Key::Enter => return RewriteOutcome::Accept(text.clone()),
                Key::Esc => return RewriteOutcome::Close,
                _ => {}
            },
        }
        RewriteOutcome::Open
    }

    /// The daemon answered (or the call failed).
    pub fn finish(&mut self, answer: Result<(AiOutcome, Option<String>), String>) {
        match answer {
            Ok((AiOutcome::Ok, Some(text))) => self.phase = Phase::Preview(text),
            Ok((outcome, _)) => {
                self.phase = Phase::Choose;
                self.message = Some(outcome_message(outcome).into());
            }
            Err(e) => {
                self.phase = Phase::Choose;
                self.message = Some(e);
            }
        }
    }
}

/// One line for an outcome that gave no text.
#[must_use]
pub fn outcome_message(outcome: AiOutcome) -> &'static str {
    match outcome {
        AiOutcome::Ok | AiOutcome::InvalidOutput => "the model gave no usable text",
        AiOutcome::Refused => "the model refused this text",
        AiOutcome::UnsupportedLanguage => {
            "the local model cannot read this language (set up translate.json)"
        }
        AiOutcome::Timeout => "the model took too long",
        AiOutcome::Unavailable => "the local model is not available (is apfel on?)",
        AiOutcome::TooLong => "the text is too long for the model",
        AiOutcome::Busy => "the model is busy, try again in a moment",
        AiOutcome::Gated | AiOutcome::Error => "the model failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(draft: &str) -> RewriteForm {
        RewriteForm::new("t1", draft)
    }

    #[test]
    fn a_choice_starts_the_request_with_its_mode() {
        let mut f = form("hello");
        f.handle(Key::Down);
        f.handle(Key::Down);
        assert_eq!(
            f.handle(Key::Enter),
            RewriteOutcome::Start {
                mode: RewriteMode::Formal,
                instruction: None
            }
        );
        assert_eq!(f.phase, Phase::Working);
    }

    #[test]
    fn a_digit_picks_directly() {
        let mut f = form("hello");
        assert_eq!(
            f.handle(Key::Char('2')),
            RewriteOutcome::Start {
                mode: RewriteMode::Grammar,
                instruction: None
            }
        );
    }

    #[test]
    fn the_custom_choice_asks_for_a_line_first() {
        let mut f = form("hello");
        assert_eq!(f.handle(Key::Enter), RewriteOutcome::Open);
        assert_eq!(f.phase, Phase::Instruction);
        // empty line: nothing is sent
        assert_eq!(f.handle(Key::Enter), RewriteOutcome::Open);
        assert!(f.message.is_some());
        for c in "make it shorter".chars() {
            f.handle(Key::Char(c));
        }
        assert_eq!(
            f.handle(Key::Enter),
            RewriteOutcome::Start {
                mode: RewriteMode::Custom,
                instruction: Some("make it shorter".into())
            }
        );
    }

    #[test]
    fn an_empty_reply_makes_no_request() {
        let mut f = form("   ");
        for key in [Key::Enter, Key::Char('3')] {
            assert_eq!(f.handle(key), RewriteOutcome::Open);
            assert_eq!(f.phase, Phase::Choose);
            assert!(f.message.as_deref().unwrap().contains("empty"));
        }
    }

    #[test]
    fn the_result_is_previewed_and_enter_takes_it() {
        let mut f = form("hello");
        f.handle(Key::Char('4'));
        f.finish(Ok((AiOutcome::Ok, Some("Hello, dear Kata.".into()))));
        assert_eq!(f.phase, Phase::Preview("Hello, dear Kata.".into()));
        assert_eq!(
            f.handle(Key::Enter),
            RewriteOutcome::Accept("Hello, dear Kata.".into())
        );
    }

    #[test]
    fn esc_on_the_result_leaves_the_reply_alone() {
        let mut f = form("hello");
        f.handle(Key::Char('4'));
        f.finish(Ok((AiOutcome::Ok, Some("x".into()))));
        assert_eq!(f.handle(Key::Esc), RewriteOutcome::Close);
    }

    #[test]
    fn a_failed_call_goes_back_to_the_choices_with_a_line() {
        for (outcome, word) in [
            (AiOutcome::Refused, "refused"),
            (AiOutcome::Timeout, "too long"),
            (AiOutcome::Unavailable, "not available"),
        ] {
            let mut f = form("hello");
            f.handle(Key::Char('2'));
            f.finish(Ok((outcome, None)));
            assert_eq!(f.phase, Phase::Choose);
            assert!(f.message.as_deref().unwrap().contains(word), "{outcome:?}");
        }
        let mut f = form("hello");
        f.handle(Key::Char('2'));
        f.finish(Err("could not reach hasted".into()));
        assert_eq!(f.message.as_deref(), Some("could not reach hasted"));
    }

    #[test]
    fn esc_closes_the_popup_from_the_choices_and_while_waiting() {
        assert_eq!(form("x").handle(Key::Esc), RewriteOutcome::Close);
        let mut f = form("x");
        f.handle(Key::Char('2'));
        assert_eq!(f.handle(Key::Esc), RewriteOutcome::Close);
    }

    #[test]
    fn the_instruction_has_a_length_limit() {
        let mut f = form("hello");
        f.handle(Key::Enter);
        for _ in 0..(INSTRUCTION_MAX + 50) {
            f.handle(Key::Char('a'));
        }
        assert_eq!(f.instruction.chars().count(), INSTRUCTION_MAX);
    }
}
