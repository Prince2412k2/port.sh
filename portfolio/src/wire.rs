//! Turning raw SSH channel bytes into the crossterm events `Shell` expects.
//!
//! Over a real terminal, crossterm reads these bytes itself from the OS. Over
//! an SSH channel there is no OS terminal to read from — `data()` on the
//! Handler hands us a byte stream directly, and this is what stands in for
//! crossterm's own parser. The escape-sequence half of this is adapted from
//! harbr's `ssh::input`, which already solved it; the addition here is SGR
//! mouse reporting (`\x1b[<...M`/`m`), which harbr's TUI never needed and this
//! one does — the map is dragged and the sheet is thrown.

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

/// Incremental decoder. Escape sequences and multi-byte UTF-8 can arrive split
/// across SSH packets, so leftovers stay buffered until the next chunk.
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
    da1: bool,
    keyboard: bool,
}

impl Decoder {
    pub fn take_da1(&mut self) -> bool {
        std::mem::take(&mut self.da1)
    }

    pub fn take_keyboard(&mut self) -> bool {
        std::mem::take(&mut self.keyboard)
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            let before = self.buf.len();
            if let Some(event) = self.next_event() {
                out.push(event);
            } else if self.buf.len() == before {
                break;
            }
        }
        out
    }

    fn next_event(&mut self) -> Option<Event> {
        let first = *self.buf.first()?;
        match first {
            0x1b => self.decode_escape(),
            b'\r' | b'\n' => {
                self.buf.drain(..1);
                Some(key(KeyCode::Enter, KeyModifiers::NONE))
            }
            0x7f | 0x08 => {
                self.buf.drain(..1);
                Some(key(KeyCode::Backspace, KeyModifiers::NONE))
            }
            b'\t' => {
                self.buf.drain(..1);
                Some(key(KeyCode::Tab, KeyModifiers::NONE))
            }
            0x03 => {
                self.buf.drain(..1);
                Some(key(KeyCode::Char('c'), KeyModifiers::CONTROL))
            }
            // The rest of Ctrl-A..Ctrl-Z. A bare control byte is represented
            // the same way real crossterm represents it: the plain letter,
            // with the modifier carrying the fact that it was a chord --
            // never a distinct "control key" variant, because `Shell::on_key`
            // matches on `KeyCode::Char('c')` guarded by the modifier, exactly
            // as it does for a key read from a real terminal.
            c @ 0x01..=0x1a => {
                self.buf.drain(..1);
                Some(key(KeyCode::Char((b'a' + c - 1) as char), KeyModifiers::CONTROL))
            }
            c if c < 0x20 => {
                self.buf.drain(..1);
                None
            }
            _ => self.decode_utf8(),
        }
    }

    fn decode_utf8(&mut self) -> Option<Event> {
        let len = utf8_len(self.buf[0]);
        if self.buf.len() < len {
            return None; // wait for the rest of the codepoint
        }
        let bytes: Vec<u8> = self.buf.drain(..len).collect();
        std::str::from_utf8(&bytes)
            .ok()
            .and_then(|s| s.chars().next())
            .map(|c| key(KeyCode::Char(c), KeyModifiers::NONE))
    }

    fn decode_escape(&mut self) -> Option<Event> {
        if self.buf.len() == 1 {
            // A lone Esc is far more common than a sequence starting one, and
            // a client sends a real sequence in one packet -- so with nothing
            // else buffered yet, it is Esc.
            self.buf.clear();
            return Some(key(KeyCode::Esc, KeyModifiers::NONE));
        }
        match self.buf[1] {
            b'[' => self.decode_csi(),
            b'O' => self.decode_ss3(),
            0x7f | 0x08 => {
                self.buf.drain(..2);
                Some(key(KeyCode::Backspace, KeyModifiers::ALT))
            }
            // Alt+<char>: folded to the bare char. Nothing in this app binds
            // an Alt chord, and losing the modifier is better than losing the
            // keystroke.
            _ => {
                self.buf.drain(..1);
                self.next_event()
            }
        }
    }

    fn decode_ss3(&mut self) -> Option<Event> {
        if self.buf.len() < 3 {
            return None;
        }
        let code = match self.buf[2] {
            b'A' => KeyCode::Up,
            b'B' => KeyCode::Down,
            b'C' => KeyCode::Right,
            b'D' => KeyCode::Left,
            b'H' => KeyCode::Home,
            b'F' => KeyCode::End,
            b'P' => KeyCode::F(1),
            b'Q' => KeyCode::F(2),
            b'R' => KeyCode::F(3),
            b'S' => KeyCode::F(4),
            _ => {
                self.buf.drain(..3);
                return None;
            }
        };
        self.buf.drain(..3);
        Some(key(code, KeyModifiers::NONE))
    }

    /// `ESC [ ... <final byte>`. Covers cursor keys, the `~`-terminated
    /// function-key family, and -- the addition over harbr's version -- SGR
    /// mouse reports, which end in `M` (press/motion) or `m` (release) and are
    /// otherwise indistinguishable from a plain CSI sequence until the params
    /// are parsed.
    fn decode_csi(&mut self) -> Option<Event> {
        if self.buf.len() > 2 && self.buf[2] == b'<' {
            return self.decode_sgr_mouse();
        }
        let end = self.buf[2..].iter().position(|b| (0x40..=0x7e).contains(b))?;
        let final_byte = self.buf[2 + end];
        let params: String = String::from_utf8_lossy(&self.buf[2..2 + end]).into_owned();
        let consumed = 3 + end;
        let modified = decode_modifiers(params.split(';').nth(1));

        let (code, modifiers) = match final_byte {
            b'A' => (Some(KeyCode::Up), modified),
            b'B' => (Some(KeyCode::Down), modified),
            b'C' => (Some(KeyCode::Right), modified),
            b'D' => (Some(KeyCode::Left), modified),
            b'H' => (Some(KeyCode::Home), modified),
            b'F' => (Some(KeyCode::End), modified),
            b'Z' => (Some(KeyCode::BackTab), KeyModifiers::SHIFT),
            b'~' => match params.split(';').next().unwrap_or("") {
                "1" | "7" => (Some(KeyCode::Home), KeyModifiers::NONE),
                "2" => (Some(KeyCode::Insert), KeyModifiers::NONE),
                "3" => (Some(KeyCode::Delete), KeyModifiers::NONE),
                "4" | "8" => (Some(KeyCode::End), KeyModifiers::NONE),
                "5" => (Some(KeyCode::PageUp), KeyModifiers::NONE),
                "6" => (Some(KeyCode::PageDown), KeyModifiers::NONE),
                "11" => (Some(KeyCode::F(1)), KeyModifiers::NONE),
                "12" => (Some(KeyCode::F(2)), KeyModifiers::NONE),
                "13" => (Some(KeyCode::F(3)), KeyModifiers::NONE),
                "14" => (Some(KeyCode::F(4)), KeyModifiers::NONE),
                "15" => (Some(KeyCode::F(5)), KeyModifiers::NONE),
                _ => (None, KeyModifiers::NONE),
            },
            b'u' if params.starts_with('?') => {
                self.keyboard = true;
                (None, KeyModifiers::NONE)
            }
            b'u' => decode_csi_u(&params),
            b'c' if params.starts_with('?') => {
                self.da1 = true;
                (None, KeyModifiers::NONE)
            }
            _ => (None, KeyModifiers::NONE),
        };
        self.buf.drain(..consumed);
        code.map(|c| key(c, modifiers))
    }

    /// `ESC [ < Cb ; Cx ; Cy (M|m)`. `Cb` packs the button and the modifiers;
    /// bit 5 (32) set means motion, i.e. a drag rather than a click.
    fn decode_sgr_mouse(&mut self) -> Option<Event> {
        let end = self.buf[3..].iter().position(|b| *b == b'M' || *b == b'm')?;
        let body = String::from_utf8_lossy(&self.buf[3..3 + end]).into_owned();
        let release = self.buf[3 + end] == b'm';
        let consumed = 4 + end;

        let mut parts = body.split(';');
        let (cb, cx, cy) = (
            parts.next()?.parse::<i64>().ok()?,
            parts.next()?.parse::<u16>().ok()?,
            parts.next()?.parse::<u16>().ok()?,
        );
        self.buf.drain(..consumed);

        // SGR coordinates are 1-based; ratatui's are 0-based.
        let (column, row) = (cx.saturating_sub(1), cy.saturating_sub(1));
        const MOTION: i64 = 32;
        const WHEEL: i64 = 64;
        let kind = if cb & WHEEL != 0 {
            if cb & 1 != 0 { MouseEventKind::ScrollDown } else { MouseEventKind::ScrollUp }
        } else if cb & MOTION != 0 {
            MouseEventKind::Drag(MouseButton::Left)
        } else if release {
            MouseEventKind::Up(MouseButton::Left)
        } else {
            match cb & 0b11 {
                0 => MouseEventKind::Down(MouseButton::Left),
                1 => MouseEventKind::Down(MouseButton::Middle),
                2 => MouseEventKind::Down(MouseButton::Right),
                _ => MouseEventKind::Moved,
            }
        };
        // The modifier bits, which were being thrown away. `Cb` carries them
        // alongside the button -- shift 4, alt 8, ctrl 16 -- and without reading
        // them a ctrl-wheel arrives as a plain wheel, so "zoom the map" and
        // "scroll the transcript" are the same event.
        const SHIFT: i64 = 4;
        const ALT: i64 = 8;
        const CTRL: i64 = 16;
        let mut modifiers = KeyModifiers::NONE;
        if cb & SHIFT != 0 {
            modifiers |= KeyModifiers::SHIFT;
        }
        if cb & ALT != 0 {
            modifiers |= KeyModifiers::ALT;
        }
        if cb & CTRL != 0 {
            modifiers |= KeyModifiers::CONTROL;
        }
        Some(Event::Mouse(MouseEvent { kind, column, row, modifiers }))
    }
}

fn decode_csi_u(params: &str) -> (Option<KeyCode>, KeyModifiers) {
    let mut parts = params.split(';');
    let code = parts.next().and_then(|part| part.parse::<u32>().ok());
    let modifiers = decode_modifiers(parts.next());
    let key = match code {
        Some(13) => Some(KeyCode::Enter),
        Some(8 | 127) => Some(KeyCode::Backspace),
        Some(9) => Some(KeyCode::Tab),
        Some(27) => Some(KeyCode::Esc),
        Some(code) => char::from_u32(code).map(KeyCode::Char),
        None => None,
    };
    (key, modifiers)
}

fn decode_modifiers(encoded: Option<&str>) -> KeyModifiers {
    let encoded = encoded
        .and_then(|part| part.split(':').next())
        .and_then(|part| part.parse::<u8>().ok())
        .unwrap_or(1)
        .saturating_sub(1);
    let mut modifiers = KeyModifiers::NONE;
    if encoded & 1 != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if encoded & 2 != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if encoded & 4 != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }
    modifiers
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

fn utf8_len(byte: u8) -> usize {
    match byte {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// Enable the modes the client needs to send: any-motion mouse tracking (1003)
/// and SGR extended coordinates (1006, so columns past 223 do not wrap).
pub const ENABLE_MOUSE: &[u8] = b"\x1b[?1003h\x1b[?1006h";
pub const DISABLE_MOUSE: &[u8] = b"\x1b[?1003l\x1b[?1006l";
pub const ENABLE_KEYS: &[u8] = b"\x1b[>1u";
pub const DISABLE_KEYS: &[u8] = b"\x1b[<u";

#[cfg(test)]
mod tests {

    /// A ctrl-wheel is not a plain wheel.
    ///
    /// `Cb` packs the modifiers in with the button and this decoder was
    /// dropping them, so holding ctrl and scrolling was indistinguishable from
    /// scrolling -- which made "zoom the map" and "scroll the words" the same
    /// event and forced the two apart by pointer position instead.
    #[test]
    fn a_wheel_carries_the_modifiers_it_was_sent_with() {
        let evs = |s: &str| Decoder::default().feed(s.as_bytes());
        let of = |s: &str| match evs(s).first() {
            Some(Event::Mouse(m)) => (m.kind, m.modifiers),
            other => panic!("not a mouse event: {other:?}"),
        };

        // 64 is wheel-up, and the low bit picks the direction.
        let (kind, mods) = of("\x1b[<64;10;5M");
        assert_eq!(kind, MouseEventKind::ScrollUp);
        assert_eq!(mods, KeyModifiers::NONE);

        // 64 + 16 = ctrl held.
        let (kind, mods) = of("\x1b[<80;10;5M");
        assert_eq!(kind, MouseEventKind::ScrollUp, "ctrl stopped it being a wheel");
        assert_eq!(mods, KeyModifiers::CONTROL);

        // 65 + 16 = ctrl and scrolling down.
        let (kind, mods) = of("\x1b[<81;10;5M");
        assert_eq!(kind, MouseEventKind::ScrollDown);
        assert_eq!(mods, KeyModifiers::CONTROL);

        // Shift and alt come through too, and together.
        let (_, mods) = of("\x1b[<68;1;1M");
        assert_eq!(mods, KeyModifiers::SHIFT);
        let (_, mods) = of("\x1b[<88;1;1M");
        assert_eq!(mods, KeyModifiers::CONTROL | KeyModifiers::ALT);
    }
    use super::*;

    fn keys(d: &mut Decoder, bytes: &[u8]) -> Vec<Event> {
        d.feed(bytes)
    }

    #[test]
    fn decodes_the_common_keys() {
        let mut d = Decoder::default();
        assert_eq!(keys(&mut d, b"a"), vec![Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE))]);
        assert_eq!(keys(&mut d, b"\x1b[A"), vec![Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))]);
        assert_eq!(keys(&mut d, b"\x1b[6~"), vec![Event::Key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))]);
        assert_eq!(keys(&mut d, b"\r"), vec![Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))]);
    }

    #[test]
    fn modified_editing_keys_keep_their_modifiers() {
        let mut d = Decoder::default();
        assert_eq!(
            keys(&mut d, b"\x1b[13;2u\x1b[127;5u\x1b[127;3u"),
            vec![
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
                Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL)),
                Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT)),
            ]
        );
        assert_eq!(
            keys(&mut d, b"\x1b\x7f"),
            vec![Event::Key(KeyEvent::new(
                KeyCode::Backspace,
                KeyModifiers::ALT
            ))]
        );
        assert_eq!(
            keys(&mut d, b"\x1b[117;5u\x1b[1;5D"),
            vec![
                Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
                Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL)),
            ]
        );
    }

    #[test]
    fn a_primary_device_reply_is_capability_data_not_input() {
        let mut d = Decoder::default();
        assert!(d.feed(b"\x1b[?1;2c\x1b[?5u").is_empty());
        assert!(d.take_da1());
        assert!(!d.take_da1());
        assert!(d.take_keyboard());
    }

    /// The one thing `Shell::on_key` actually branches on for this byte: a
    /// plain Char('c') with the CONTROL modifier, the same shape a real
    /// terminal produces -- not a distinct "control key" variant.
    #[test]
    fn ctrl_c_is_a_control_modified_char_not_a_special_variant() {
        let mut d = Decoder::default();
        let evs = keys(&mut d, b"\x03");
        assert_eq!(evs, vec![Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))]);
    }

    #[test]
    fn handles_split_sequences() {
        let mut d = Decoder::default();
        assert!(keys(&mut d, b"\x1b[").is_empty());
        assert_eq!(keys(&mut d, b"C"), vec![Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))]);
    }

    #[test]
    fn handles_multibyte_chars_split_across_feeds() {
        let mut d = Decoder::default();
        let bytes = "é".as_bytes();
        assert!(keys(&mut d, &bytes[..1]).is_empty());
        assert_eq!(keys(&mut d, &bytes[1..]), vec![Event::Key(KeyEvent::new(KeyCode::Char('é'), KeyModifiers::NONE))]);
    }

    #[test]
    fn a_burst_decodes_to_several_events_in_order() {
        let mut d = Decoder::default();
        assert_eq!(
            keys(&mut d, b"j\x1b[Bq"),
            vec![
                Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
                Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
                Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            ]
        );
    }

    #[test]
    fn sgr_wheel_and_drag_decode_to_the_right_mouse_events() {
        let mut d = Decoder::default();
        // Wheel down at (10, 5), 1-based on the wire.
        assert_eq!(
            keys(&mut d, b"\x1b[<65;11;6M"),
            vec![Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 10,
                row: 5,
                modifiers: KeyModifiers::NONE,
            })]
        );
        // A left-button press, then motion (drag) while still held.
        assert_eq!(
            keys(&mut d, b"\x1b[<0;1;1M"),
            vec![Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            })]
        );
        assert_eq!(
            keys(&mut d, b"\x1b[<32;5;5M"),
            vec![Event::Mouse(MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: 4,
                row: 4,
                modifiers: KeyModifiers::NONE,
            })]
        );
        // Release: same button field, lowercase terminator.
        assert_eq!(
            keys(&mut d, b"\x1b[<0;5;5m"),
            vec![Event::Mouse(MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: 4,
                row: 4,
                modifiers: KeyModifiers::NONE,
            })]
        );
    }

    #[test]
    fn a_mouse_sequence_split_mid_packet_still_decodes() {
        let mut d = Decoder::default();
        assert!(keys(&mut d, b"\x1b[<0;1").is_empty());
        assert_eq!(
            keys(&mut d, b";1M"),
            vec![Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            })]
        );
    }
}
