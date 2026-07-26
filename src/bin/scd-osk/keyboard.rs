use evdev::KeyCode;
use scd::{OskPadSide, OskState};
use std::sync::mpsc::Sender;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyStroke {
    pub code: u16,
    pub shift: bool,
    pub session: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Half {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Slot {
    pub row: u8,
    pub key: u8,
}

#[derive(Default)]
pub struct Keyboard {
    page: Page,
    shifted: bool,
    pointers: [Option<[f32; 2]>; 2],
    pressed: [bool; 2],
    click_cursor: u64,
    visible: bool,
    initialized: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Page {
    #[default]
    Letters,
    Symbols,
}

#[derive(Clone, Copy)]
struct Key {
    label: &'static str,
    shifted_label: Option<&'static str>,
    action: Action,
    weight: u8,
}

#[derive(Clone, Copy)]
enum Action {
    Key { code: KeyCode, shift: Shift },
    Shift,
    Page(Page),
}

#[derive(Clone, Copy)]
enum Shift {
    Never,
    Always,
    Latch,
}

macro_rules! character {
    ($label:literal, $shifted:literal, $code:ident) => {
        Key {
            label: $label,
            shifted_label: Some($shifted),
            action: Action::Key {
                code: KeyCode::$code,
                shift: Shift::Latch,
            },
            weight: 10,
        }
    };
}

macro_rules! key {
    ($label:literal, $code:ident) => {
        Key {
            label: $label,
            shifted_label: None,
            action: Action::Key {
                code: KeyCode::$code,
                shift: Shift::Never,
            },
            weight: 10,
        }
    };
    ($label:literal, $code:ident, shift) => {
        Key {
            label: $label,
            shifted_label: None,
            action: Action::Key {
                code: KeyCode::$code,
                shift: Shift::Always,
            },
            weight: 10,
        }
    };
    ($label:literal, $code:ident, $weight:literal) => {
        Key {
            label: $label,
            shifted_label: None,
            action: Action::Key {
                code: KeyCode::$code,
                shift: Shift::Never,
            },
            weight: $weight,
        }
    };
}

const SHIFT: Key = Key {
    label: "Shift",
    shifted_label: None,
    action: Action::Shift,
    weight: 20,
};
const CAPS: Key = key!("Caps", KEY_CAPSLOCK, 16);
const SYMBOLS: Key = Key {
    label: "?123",
    shifted_label: None,
    action: Action::Page(Page::Symbols),
    weight: 14,
};
const ALPHABET: Key = Key {
    label: "ABC",
    shifted_label: None,
    action: Action::Page(Page::Letters),
    weight: 14,
};
const TAB: Key = key!("Tab", KEY_TAB, 14);
const SPACE: Key = key!("Space", KEY_SPACE, 60);
const BACKSPACE: Key = key!("Backspace", KEY_BACKSPACE, 14);
const ENTER: Key = key!("Enter", KEY_ENTER, 17);

const LETTERS_PAGE: [&[Key]; 5] = [
    &[
        character!("`", "~", KEY_GRAVE),
        character!("1", "!", KEY_1),
        character!("2", "@", KEY_2),
        character!("3", "#", KEY_3),
        character!("4", "$", KEY_4),
        character!("5", "%", KEY_5),
        character!("6", "^", KEY_6),
        character!("7", "&", KEY_7),
        character!("8", "*", KEY_8),
        character!("9", "(", KEY_9),
        character!("0", ")", KEY_0),
        character!("-", "_", KEY_MINUS),
        character!("=", "+", KEY_EQUAL),
        BACKSPACE,
    ],
    &[
        TAB,
        character!("q", "Q", KEY_Q),
        character!("w", "W", KEY_W),
        character!("e", "E", KEY_E),
        character!("r", "R", KEY_R),
        character!("t", "T", KEY_T),
        character!("y", "Y", KEY_Y),
        character!("u", "U", KEY_U),
        character!("i", "I", KEY_I),
        character!("o", "O", KEY_O),
        character!("p", "P", KEY_P),
        character!("[", "{", KEY_LEFTBRACE),
        character!("]", "}", KEY_RIGHTBRACE),
        character!("\\", "|", KEY_BACKSLASH),
    ],
    &[
        CAPS,
        character!("a", "A", KEY_A),
        character!("s", "S", KEY_S),
        character!("d", "D", KEY_D),
        character!("f", "F", KEY_F),
        character!("g", "G", KEY_G),
        character!("h", "H", KEY_H),
        character!("j", "J", KEY_J),
        character!("k", "K", KEY_K),
        character!("l", "L", KEY_L),
        character!(";", ":", KEY_SEMICOLON),
        character!("'", "\"", KEY_APOSTROPHE),
        ENTER,
    ],
    &[
        SHIFT,
        character!("z", "Z", KEY_Z),
        character!("x", "X", KEY_X),
        character!("c", "C", KEY_C),
        character!("v", "V", KEY_V),
        character!("b", "B", KEY_B),
        character!("n", "N", KEY_N),
        character!("m", "M", KEY_M),
        character!(",", "<", KEY_COMMA),
        character!(".", ">", KEY_DOT),
        character!("/", "?", KEY_SLASH),
        SHIFT,
    ],
    &[SYMBOLS, TAB, SPACE, BACKSPACE, ENTER, SYMBOLS],
];

const SYMBOLS_PAGE: [&[Key]; 5] = [
    &[
        key!("1", KEY_1),
        key!("2", KEY_2),
        key!("3", KEY_3),
        key!("4", KEY_4),
        key!("5", KEY_5),
        key!("6", KEY_6),
        key!("7", KEY_7),
        key!("8", KEY_8),
        key!("9", KEY_9),
        key!("0", KEY_0),
    ],
    &[
        key!("!", KEY_1, shift),
        key!("@", KEY_2, shift),
        key!("#", KEY_3, shift),
        key!("$", KEY_4, shift),
        key!("%", KEY_5, shift),
        key!("^", KEY_6, shift),
        key!("&", KEY_7, shift),
        key!("*", KEY_8, shift),
        key!("(", KEY_9, shift),
        key!(")", KEY_0, shift),
    ],
    &[
        key!("`", KEY_GRAVE),
        key!("~", KEY_GRAVE, shift),
        key!("-", KEY_MINUS),
        key!("_", KEY_MINUS, shift),
        key!("=", KEY_EQUAL),
        key!("+", KEY_EQUAL, shift),
        key!("[", KEY_LEFTBRACE),
        key!("]", KEY_RIGHTBRACE),
        key!("{", KEY_LEFTBRACE, shift),
        key!("}", KEY_RIGHTBRACE, shift),
    ],
    &[
        key!(";", KEY_SEMICOLON),
        key!(":", KEY_SEMICOLON, shift),
        key!("'", KEY_APOSTROPHE),
        key!("\"", KEY_APOSTROPHE, shift),
        key!("/", KEY_SLASH),
        key!("?", KEY_SLASH, shift),
        key!(",", KEY_COMMA),
        key!(".", KEY_DOT),
        key!("<", KEY_COMMA, shift),
        key!(">", KEY_DOT, shift),
        key!("\\", KEY_BACKSLASH),
        key!("|", KEY_BACKSLASH, shift),
    ],
    &[ALPHABET, TAB, SPACE, BACKSPACE, ENTER, ALPHABET],
];

impl Keyboard {
    pub fn update(&mut self, state: OskState, output: &Sender<KeyStroke>) -> bool {
        let before = (self.page, self.shifted, self.pointers, self.pressed);
        let accept_clicks = self.initialized && (state.visible || self.visible);
        if !self.initialized {
            self.initialized = true;
        }

        let clicks = state.clicks_since(self.click_cursor);
        if accept_clicks && clicks.missed() != 0 {
            log::warn!("keyboard input missed {} clicks", clicks.missed());
        }
        for click in clicks.filter(|_| accept_clicks) {
            let half = match click.pad {
                OskPadSide::Left => Half::Left,
                OskPadSide::Right => Half::Right,
            };
            let Some(action) = hit(self.page, half, click.position) else {
                continue;
            };
            match action {
                Action::Key { code, shift } => {
                    let shift = match shift {
                        Shift::Never => false,
                        Shift::Always => true,
                        Shift::Latch => self.shifted,
                    };
                    if output
                        .send(KeyStroke {
                            code: code.code(),
                            shift,
                            session: state.session(),
                        })
                        .is_err()
                    {
                        log::error!("keyboard output worker stopped");
                    }
                    if matches!(
                        action,
                        Action::Key {
                            shift: Shift::Latch,
                            ..
                        }
                    ) {
                        self.shifted = false;
                    }
                }
                Action::Shift => self.shifted = !self.shifted,
                Action::Page(page) => {
                    self.page = page;
                    self.shifted = false;
                }
            }
        }
        self.click_cursor = state.click_cursor();

        self.pointers = if state.visible {
            [
                state.left.touched.then_some(state.left.position),
                state.right.touched.then_some(state.right.position),
            ]
        } else {
            [None, None]
        };
        self.pressed = if state.visible {
            [state.left.pressed, state.right.pressed]
        } else {
            [false, false]
        };
        self.visible = state.visible;
        before != (self.page, self.shifted, self.pointers, self.pressed)
    }

    pub fn disconnect(&mut self) -> bool {
        let changed = self.pointers != [None, None] || self.pressed != [false, false];
        self.initialized = false;
        self.visible = false;
        self.pointers = [None, None];
        self.pressed = [false, false];
        changed
    }

    pub fn for_each_key(
        &self,
        width: f32,
        height: f32,
        mut visit: impl FnMut(Slot, &'static str, Option<&'static str>, [f32; 4], bool, bool),
    ) {
        let rows = rows(self.page);
        let row_height = height / rows.len() as f32;
        for (row_index, row) in rows.iter().enumerate() {
            let total_weight = row.iter().map(|key| u32::from(key.weight)).sum::<u32>();
            let mut x = 0.0;
            for (key_index, key) in row.iter().enumerate() {
                let key_width = width * f32::from(key.weight) / total_weight as f32;
                let shifted_label = key.shifted_label.filter(|_| {
                    key.label.len() != 1 || !key.label.as_bytes()[0].is_ascii_alphabetic()
                });
                let (label, secondary_label) = match (self.shifted, key.shifted_label) {
                    (true, Some(shifted)) => (shifted, shifted_label.map(|_| key.label)),
                    _ => (key.label, shifted_label),
                };
                let active = matches!(key.action, Action::Shift) && self.shifted;
                let special =
                    matches!(key.action, Action::Shift | Action::Page(_)) || key.label.len() > 2;
                visit(
                    Slot {
                        row: row_index as u8,
                        key: key_index as u8,
                    },
                    label,
                    secondary_label,
                    [x, row_index as f32 * row_height, key_width, row_height],
                    active,
                    special,
                );
                x += key_width;
            }
        }
    }

    pub fn pointer(&self, half: Half) -> Option<[f32; 2]> {
        self.pointers[usize::from(half == Half::Right)]
    }

    pub fn pressed(&self, half: Half) -> bool {
        self.pressed[usize::from(half == Half::Right)]
    }
}

fn hit(page: Page, half: Half, position: [f32; 2]) -> Option<Action> {
    let slot = hit_slot(page, half, position)?;
    Some(rows(page)[usize::from(slot.row)][usize::from(slot.key)].action)
}

fn hit_slot(page: Page, half: Half, position: [f32; 2]) -> Option<Slot> {
    if !position.into_iter().all(f32::is_finite) {
        return None;
    }
    let rows = rows(page);
    let y = ((position[1].clamp(-1.0, 1.0) + 1.0) * 0.5 * rows.len() as f32)
        .floor()
        .min(rows.len() as f32 - 1.0) as usize;
    let row = rows[y];
    let total_weight = row.iter().map(|key| u32::from(key.weight)).sum::<u32>();
    let target = ((position[0].clamp(-1.0, 1.0) + 1.0) * 0.25
        + if half == Half::Left { 0.0 } else { 0.5 })
        * total_weight as f32;
    let mut boundary = 0.0;
    for (key, spec) in row.iter().enumerate() {
        boundary += f32::from(spec.weight);
        if target < boundary || key + 1 == row.len() {
            return Some(Slot {
                row: y as u8,
                key: key as u8,
            });
        }
    }
    None
}

fn rows(page: Page) -> &'static [&'static [Key]; 5] {
    match page {
        Page::Letters => &LETTERS_PAGE,
        Page::Symbols => &SYMBOLS_PAGE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replays_coalesced_clicks_in_order_through_close() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut keyboard = Keyboard::default();
        let mut state = OskState::default();
        state.set_visible(true);
        keyboard.update(state, &sender);

        state.record_click(OskPadSide::Left, [-0.5, -0.5]);
        state.record_click(OskPadSide::Right, [-1.0, -0.5]);
        state.record_click(OskPadSide::Left, [-0.2, -0.5]);
        state.set_visible(false);
        keyboard.update(state, &sender);

        for expected in [KeyCode::KEY_Q, KeyCode::KEY_Y, KeyCode::KEY_W] {
            assert_eq!(
                receiver.try_recv(),
                Ok(KeyStroke {
                    code: expected.code(),
                    shift: false,
                    session: 1,
                })
            );
        }
        assert_eq!(
            receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        );
    }

    #[test]
    fn symbols_and_shift_apply_in_click_order() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut keyboard = Keyboard::default();
        let mut state = OskState::default();
        state.set_visible(true);
        keyboard.update(state, &sender);

        for position in [
            [-0.9, 0.9],
            [-0.9, -0.5],
            [-0.9, 0.9],
            [-0.9, 0.3],
            [-0.5, -0.5],
            [-0.5, -0.5],
        ] {
            state.record_click(OskPadSide::Left, position);
        }
        keyboard.update(state, &sender);

        for (code, shift) in [
            (KeyCode::KEY_1, true),
            (KeyCode::KEY_Q, true),
            (KeyCode::KEY_Q, false),
        ] {
            assert_eq!(
                receiver.try_recv(),
                Ok(KeyStroke {
                    code: code.code(),
                    shift,
                    session: 1,
                })
            );
        }
        assert_eq!(
            receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        );
    }

    #[test]
    fn pointer_motion_invalidates_without_changing_the_key() {
        let (sender, _) = std::sync::mpsc::channel();
        let mut keyboard = Keyboard::default();
        let mut state = OskState::default();
        state.set_visible(true);
        state.left.touched = true;
        state.left.position = [-0.8, -0.5];

        assert!(keyboard.update(state, &sender));
        assert_eq!(keyboard.pointer(Half::Left), Some([-0.8, -0.5]));
        assert!(!keyboard.update(state, &sender));

        state.left.position[0] = -0.7;
        assert!(keyboard.update(state, &sender));
        assert_eq!(keyboard.pointer(Half::Left), Some([-0.7, -0.5]));
        assert!(keyboard.disconnect());
        assert_eq!(keyboard.pointer(Half::Left), None);
    }
}
