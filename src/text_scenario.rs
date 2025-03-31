use std::{sync::Arc, time::Duration};

use super::Action;

#[derive(Clone, Copy)]
pub struct ParseError {
    pub position: usize,
    /// Character that was not expected, or None if EOF was not expected.
    pub chararter: Option<u8>,
    pub state: &'static str,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.chararter {
            Some(c) => write!(
                f,
                "Unexpected character `{}` at position {} while {}",
                std::ascii::escape_default(c),
                self.position,
                self.state
            ),
            None => write!(f, "Unexpected EOF while {}", self.state),
        }
    }
}

impl std::fmt::Debug for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self, f)
    }
}

pub(crate) fn parse_text_scenario(
    text_scenario: &str,
) -> Result<(Vec<Action>, Option<String>), ParseError> {
    #[derive(Copy, Clone)]
    enum BufferMode {
        Read,
        Write,
        ReadZeroes,
        WriteZeroes,
        IgnoreWritten,
        SetName,
        WaitMilliseconds,
    }

    #[derive(Copy, Clone, Debug)]
    enum ParserState {
        WaitingForCommandCharacter,
        JustAfterCommandCharacter,
        InjectError,
        NumberFirst,
        NumberDec,
        NumberHex,
        Content,
        Escape,
        HexEscape1,
        HexEscape2(u8),
        Zero,
    }

    impl ParserState {
        fn str(&self) -> &'static str {
            match self {
                WaitingForCommandCharacter => "waiting for a command character",
                JustAfterCommandCharacter => {
                    "scanning read or write buffer content or {whitespace after command character that is to be skipped}"
                }
                InjectError => "waiting for R or W character to choose which error to inject",
                NumberFirst => "waiting for a number",
                NumberDec => "continuing a decimal number",
                NumberHex => "continuing a hexadecimal number",
                Content => "scanning read or write buffer content",
                Escape => "interpreting a character after `\\`",
                HexEscape1 => "interpreting a hex escape, first letter",
                HexEscape2(_) => "interpreting a hex escape, second letter",
                Zero => "waiting for a subcommand character ater `Z`",
            }
        }
    }
    let mut buf = vec![];
    let mut numbuf: u64 = 0;
    let mut bufmode = BufferMode::Read;
    let mut state = ParserState::WaitingForCommandCharacter;

    let mut set_name = None;

    let mut actions = vec![];
    let mut position = 0usize;

    macro_rules! commit_buffer {
        () => {
            match bufmode {
                BufferMode::Write => {
                    actions.push(Action::Write(std::mem::take(&mut buf)));
                }
                BufferMode::Read => {
                    actions.push(Action::Read(std::mem::take(&mut buf)));
                }
                BufferMode::ReadZeroes => {
                    actions.push(Action::ReadZeroes(numbuf as usize));
                }
                BufferMode::WriteZeroes => {
                    actions.push(Action::WriteZeroes(numbuf as usize));
                }
                BufferMode::IgnoreWritten => {
                    actions.push(Action::IgnoreWritten(numbuf as usize));
                }
                BufferMode::SetName => {
                    set_name = Some(String::from_utf8_lossy(&buf[..]).to_string());
                }
                BufferMode::WaitMilliseconds => {
                    actions.push(Action::Wait(Duration::from_millis(numbuf)));
                }
            }
            buf.clear();
            #[allow(unused_assignments)]
            {
                numbuf = 0;
            }
        };
    }

    use ParserState::*;
    for b in text_scenario.bytes() {
        match (state, b) {
            (
                WaitingForCommandCharacter
                | JustAfterCommandCharacter
                | InjectError
                | NumberFirst
                | NumberHex
                | NumberDec
                | Zero,
                b' ' | b'\t' | b'\n',
            ) => {}
            (WaitingForCommandCharacter, b'R' | b'r') => {
                buf.clear();
                bufmode = BufferMode::Read;
                state = JustAfterCommandCharacter;
            }
            (WaitingForCommandCharacter, b'W' | b'w') => {
                buf.clear();
                bufmode = BufferMode::Write;
                state = JustAfterCommandCharacter;
            }
            (WaitingForCommandCharacter, b'|') => {}
            (WaitingForCommandCharacter, b'E') => {
                state = InjectError;
            }
            (WaitingForCommandCharacter, b'T') => {
                bufmode = BufferMode::WaitMilliseconds;
                state = NumberFirst;
            }
            (WaitingForCommandCharacter, b'N' | b'n') => {
                buf.clear();
                bufmode = BufferMode::SetName;
                state = JustAfterCommandCharacter;
            }
            (WaitingForCommandCharacter, b'z' | b'Z') => {
                state = Zero;
            }
            (WaitingForCommandCharacter, b'X' | b'x') => {
                actions.push(Action::ReadEof(false));
                state=WaitingForCommandCharacter;
            }
            (WaitingForCommandCharacter, b'Q' | b'q') => {
                actions.push(Action::StopChecking);
                state=WaitingForCommandCharacter;
            }
            (WaitingForCommandCharacter, b'i' | b'I') => {
                bufmode = BufferMode::IgnoreWritten;
                state = NumberFirst;
            }
            (JustAfterCommandCharacter | Content, b'|') => {
                commit_buffer!();
                state = WaitingForCommandCharacter;
            }
            (InjectError, b'R' | b'r') => {
                actions.push(Action::ReadError(Some(Arc::new(
                    std::io::ErrorKind::Other.into(),
                ))));
            }
            (InjectError, b'W' | b'w') => {
                actions.push(Action::WriteError(Some(Arc::new(
                    std::io::ErrorKind::Other.into(),
                ))));
            }
            (InjectError, b'|') => {
                state = WaitingForCommandCharacter;
            }
            (JustAfterCommandCharacter | Content, b'\\') => {
                state = Escape;
            }
            (JustAfterCommandCharacter | Content, b) => {
                buf.push(b);
                state = Content;
            }
            (Zero, b'r' | b'R') => {
                bufmode = BufferMode::ReadZeroes;
                state = NumberFirst;
            }
            (Zero, b'w' | b'W') => {
                bufmode = BufferMode::WriteZeroes;
                state = NumberFirst;
            }
            (Escape, b'n') => {
                buf.push(b'\n');
                state = Content;
            }
            (Escape, b'r') => {
                buf.push(b'\r');
                state = Content;
            }
            (Escape, b'0') => {
                buf.push(b'\0');
                state = Content;
            }
            (Escape, b't') => {
                buf.push(b'\t');
                state = Content;
            }
            (Escape, b'\\') => {
                buf.push(b'\\');
                state = Content;
            }
            (Escape, b'|') => {
                buf.push(b'|');
                state = Content;
            }
            (Escape, b'x') => {
                state = HexEscape1;
            }
            (HexEscape1, x @ (b'0'..=b'9' | b'A'..=b'F' | b'a'..=b'f')) => {
                state = HexEscape2(x);
            }
            (HexEscape2(c1), c2 @ (b'0'..=b'9' | b'A'..=b'F' | b'a'..=b'f')) => {
                let mut b = [0];
                let s = [c1, c2];
                hex::decode_to_slice(s, &mut b).unwrap();
                buf.push(b[0]);
                state = Content;
            }
            (Escape, b) => {
                buf.push(b);
                state = Content;
            }
            (NumberFirst | NumberDec, b @ (b'0'..=b'9')) => {
                let Some(Some(x)) = numbuf
                    .checked_mul(10)
                    .map(|x| x.checked_add((b - b'0').into()))
                else {
                    return Err(ParseError {
                        position,
                        chararter: Some(b),
                        state: "continuing a decimal number (which overflowed)",
                    });
                };
                numbuf = x;
                if numbuf > 0 {
                    state = NumberDec;
                }
            }
            (NumberFirst, b'x' | b'X') => {
                state = NumberHex;
            }
            (NumberHex, b @ (b'0'..=b'9') | b @ (b'a'..=b'f') | b @ (b'A'..=b'F')) => {
                let mut hx = [0u8; 1];
                hex::decode_to_slice(&[b'0', b], &mut hx).unwrap();
                let Some(Some(x)) = numbuf
                    .checked_mul(16)
                    .map(|x| x.checked_add((hx[0]).into()))
                else {
                    return Err(ParseError {
                        position,
                        chararter: Some(b),
                        state: "continuing a hex number (which overflowed)",
                    });
                };
                numbuf = x;
            }
            (NumberFirst | NumberHex | NumberDec, b'|') => {
                commit_buffer!();
                state = WaitingForCommandCharacter;
            }
            (s, b) => {
                return Err(ParseError {
                    position,
                    chararter: Some(b),
                    state: s.str(),
                });
            }
        }
        position += 1;
    }

    match state {
        WaitingForCommandCharacter | InjectError => {}
        JustAfterCommandCharacter | Content | NumberHex | NumberDec | NumberFirst => {
            commit_buffer!();
        }
        s => {
            return Err(ParseError {
                position,
                chararter: None,
                state: s.str(),
            });
        }
    }

    Ok((actions, set_name))
}

#[cfg(test)]
fn summarize_actions<'a>(actions: impl IntoIterator<Item = &'a Action>) -> String {
    let mut s = String::with_capacity(64);
    for a in actions {
        match a {
            Action::Read(vec) => s.push_str(&format!("r{}", vec.len())),
            Action::Write(vec) => s.push_str(&format!("w{}", vec.len())),
            Action::Wait(_duration) => s.push_str("t"),
            Action::ReadError(_error) => s.push_str("ER"),
            Action::WriteError(_error) => s.push_str("EW"),
            Action::ReadZeroes(n) => s.push_str(&format!("ZR{}", n)),
            Action::WriteZeroes(n) => s.push_str(&format!("ZW{}", n)),
            Action::IgnoreWritten(n) => s.push_str(&format!("I{}", n)),
            Action::ReadEof(_) => s.push_str("X"),
            Action::StopChecking => s.push_str("Q"),
        }
    }
    s
}

#[test]
fn text_scenaio_1() {
    let ret = parse_text_scenario("R qqq|W \\x01\\x02\\x03|R \\ \\n\\t\\0\\r\\\\\\|").unwrap();
    assert_eq!(summarize_actions(&ret.0), "r3w3r7");
    assert_eq!(ret.1, None);
    assert!(matches!(&ret.0[0], Action::Read(v) if v == b"qqq"));
    assert!(matches!(&ret.0[1], Action::Write(v) if v == b"\x01\x02\x03"));
    assert!(matches!(&ret.0[2], Action::Read(v) if v == b" \n\t\x00\r\\|"));
}

#[test]
fn text_scenaio_2() {
    let ret = parse_text_scenario("T4|ER|EW|N qwerty").unwrap();
    assert_eq!(summarize_actions(&ret.0), "tEREW");
    assert_eq!(ret.1.as_deref(), Some("qwerty"));
}

#[test]
fn text_scenaio_3() {
    let ret = parse_text_scenario("T0|T123|Tx456|T0x112233").unwrap();
    assert_eq!(summarize_actions(&ret.0), "tttt");
    assert!(matches!(&ret.0[0], Action::Wait(t) if t.as_millis() == 0));
    assert!(matches!(&ret.0[1], Action::Wait(t) if t.as_millis() == 123));
    assert!(matches!(&ret.0[2], Action::Wait(t) if t.as_millis() == 0x456));
    assert!(matches!(&ret.0[3], Action::Wait(t) if t.as_millis() == 0x112233));
}


#[test]
fn text_scenaio_4() {
    let ret = parse_text_scenario("ZR45|ZW0|ZRx34|zw0x20").unwrap();
    assert_eq!(summarize_actions(&ret.0), "ZR45ZW0ZR52ZW32");
}



#[test]
fn text_scenaio_5() {
    let ret = parse_text_scenario("X|Q|I55").unwrap();
    assert_eq!(summarize_actions(&ret.0), "XQI55");
}
