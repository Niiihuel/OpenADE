//! Keyboard keys by the names Playwright uses (`Enter`, `ArrowLeft`, `a`,
//! `KeyA`, `Control+A`), resolved against a US layout into what
//! `Input.dispatchKeyEvent` needs.

/// Bits of the protocol's `modifiers` field.
pub const ALT: u8 = 1;
pub const CONTROL: u8 = 2;
pub const META: u8 = 4;
pub const SHIFT: u8 = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Key {
    /// The `KeyboardEvent.key` value.
    pub key: String,
    /// The physical key, `KeyboardEvent.code`. Empty for a character that is
    /// not on the layout.
    pub code: String,
    /// The Windows virtual key code, which is what shortcuts match on.
    pub key_code: u32,
    /// What the key types, if anything.
    pub text: String,
    /// 1 for the left one of a pair, 2 for the right one.
    pub location: u8,
    /// The modifier bit this key itself holds down.
    pub modifier: u8,
}

/// Keys that type nothing, or whose name is not what they type:
/// name, code, virtual key code, text.
const NAMED: &[(&str, &str, u32, &str)] = &[
    ("Backspace", "Backspace", 8, ""),
    ("Tab", "Tab", 9, ""),
    ("Enter", "Enter", 13, "\r"),
    ("Pause", "Pause", 19, ""),
    ("CapsLock", "CapsLock", 20, ""),
    ("Escape", "Escape", 27, ""),
    ("Space", "Space", 32, " "),
    (" ", "Space", 32, " "),
    ("PageUp", "PageUp", 33, ""),
    ("PageDown", "PageDown", 34, ""),
    ("End", "End", 35, ""),
    ("Home", "Home", 36, ""),
    ("ArrowLeft", "ArrowLeft", 37, ""),
    ("ArrowUp", "ArrowUp", 38, ""),
    ("ArrowRight", "ArrowRight", 39, ""),
    ("ArrowDown", "ArrowDown", 40, ""),
    ("PrintScreen", "PrintScreen", 44, ""),
    ("Insert", "Insert", 45, ""),
    ("Delete", "Delete", 46, ""),
    ("ContextMenu", "ContextMenu", 93, ""),
    ("NumLock", "NumLock", 144, ""),
    ("ScrollLock", "ScrollLock", 145, ""),
];

/// name, code, virtual key code, location, modifier bit.
const MODIFIERS: &[(&str, &str, u32, u8, u8)] = &[
    ("Shift", "ShiftLeft", 16, 1, SHIFT),
    ("ShiftLeft", "ShiftLeft", 16, 1, SHIFT),
    ("ShiftRight", "ShiftRight", 16, 2, SHIFT),
    ("Control", "ControlLeft", 17, 1, CONTROL),
    ("ControlLeft", "ControlLeft", 17, 1, CONTROL),
    ("ControlRight", "ControlRight", 17, 2, CONTROL),
    ("Alt", "AltLeft", 18, 1, ALT),
    ("AltLeft", "AltLeft", 18, 1, ALT),
    ("AltRight", "AltRight", 18, 2, ALT),
    ("Meta", "MetaLeft", 91, 1, META),
    ("MetaLeft", "MetaLeft", 91, 1, META),
    ("MetaRight", "MetaRight", 92, 2, META),
];

/// Punctuation of the US layout: code, virtual key code, plain, shifted.
const PUNCTUATION: &[(&str, u32, char, char)] = &[
    ("Backquote", 192, '`', '~'),
    ("Minus", 189, '-', '_'),
    ("Equal", 187, '=', '+'),
    ("BracketLeft", 219, '[', '{'),
    ("BracketRight", 221, ']', '}'),
    ("Backslash", 220, '\\', '|'),
    ("Semicolon", 186, ';', ':'),
    ("Quote", 222, '\'', '"'),
    ("Comma", 188, ',', '<'),
    ("Period", 190, '.', '>'),
    ("Slash", 191, '/', '?'),
];

const SHIFTED_DIGITS: &[u8; 10] = b")!@#$%^&*(";

fn typed(key: char, code: String, key_code: u32) -> Key {
    Key {
        key: key.to_string(),
        code,
        key_code,
        text: key.to_string(),
        location: 0,
        modifier: 0,
    }
}

/// Resolves one key name. `shift` picks the shifted character of a key named
/// by its position (`KeyA`, `Digit1`, `Minus`).
pub(crate) fn key(name: &str, shift: bool) -> Option<Key> {
    let name = match name {
        // The primary shortcut modifier; this driver only runs Chrome with
        // the Windows and Linux conventions.
        "ControlOrMeta" => "Control",
        other => other,
    };
    if let Some((name, code, key_code, location, modifier)) =
        MODIFIERS.iter().find(|entry| entry.0 == name)
    {
        return Some(Key {
            key: name.trim_end_matches("Left").trim_end_matches("Right").to_owned(),
            code: (*code).to_owned(),
            key_code: *key_code,
            text: String::new(),
            location: *location,
            modifier: *modifier,
        });
    }
    if let Some((name, code, key_code, text)) = NAMED.iter().find(|entry| entry.0 == name) {
        return Some(Key {
            key: if *name == "Space" { " ".to_owned() } else { (*name).to_owned() },
            code: (*code).to_owned(),
            key_code: *key_code,
            text: (*text).to_owned(),
            location: 0,
            modifier: 0,
        });
    }
    if let Some(number) = name.strip_prefix('F').and_then(|number| number.parse::<u32>().ok()) {
        return (1..=24).contains(&number).then(|| Key {
            key: name.to_owned(),
            code: name.to_owned(),
            key_code: 111 + number,
            text: String::new(),
            location: 0,
            modifier: 0,
        });
    }
    // A key named by its position on the layout.
    if let Some(letter) = name.strip_prefix("Key").filter(|letter| letter.len() == 1) {
        let letter = letter.chars().next().filter(char::is_ascii_uppercase)?;
        let character = if shift { letter } else { letter.to_ascii_lowercase() };
        return Some(typed(character, name.to_owned(), letter as u32));
    }
    if let Some(digit) = name.strip_prefix("Digit").filter(|digit| digit.len() == 1) {
        let digit = digit.bytes().next().filter(u8::is_ascii_digit)?;
        let character = if shift { SHIFTED_DIGITS[usize::from(digit - b'0')] } else { digit };
        return Some(typed(char::from(character), name.to_owned(), u32::from(digit)));
    }
    if let Some((code, key_code, plain, shifted)) = PUNCTUATION.iter().find(|entry| entry.0 == name) {
        return Some(typed(if shift { *shifted } else { *plain }, (*code).to_owned(), *key_code));
    }
    // A key named by the character it types. With Shift held it types the
    // other character of the same key.
    let mut characters = name.chars();
    let (Some(character), None) = (characters.next(), characters.next()) else {
        return None;
    };
    if character.is_ascii_alphabetic() {
        let upper = character.to_ascii_uppercase();
        let typed_character = if shift { upper } else { character };
        return Some(typed(typed_character, format!("Key{upper}"), upper as u32));
    }
    if character.is_ascii_digit() {
        let shifted = char::from(SHIFTED_DIGITS[usize::from(character as u8 - b'0')]);
        return Some(typed(if shift { shifted } else { character }, format!("Digit{character}"), character as u32));
    }
    if let Some(digit) = SHIFTED_DIGITS.iter().position(|shifted| char::from(*shifted) == character) {
        return Some(typed(character, format!("Digit{digit}"), 48 + digit as u32));
    }
    if let Some((code, key_code, plain, shifted)) = PUNCTUATION
        .iter()
        .find(|entry| entry.2 == character || entry.3 == character)
    {
        let typed_character = if shift && character == *plain { *shifted } else { character };
        return Some(typed(typed_character, (*code).to_owned(), *key_code));
    }
    // Not on this layout (ñ, é, 🧠): it still types itself.
    (!character.is_control()).then(|| typed(character, String::new(), 0))
}

/// Splits `Control+Shift+A` into its keys. A `+` is a separator only after a
/// key, so `Control++` presses the plus key and `+` alone is that key.
pub(crate) fn chord(keys: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    for (at, character) in keys.char_indices() {
        if character == '+' && at > start {
            parts.push(&keys[start..at]);
            start = at + 1;
        }
    }
    parts.push(&keys[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_split_on_plus_only_after_a_key() {
        assert_eq!(chord("Enter"), ["Enter"]);
        assert_eq!(chord("Control+A"), ["Control", "A"]);
        assert_eq!(chord("Control+Shift+ArrowLeft"), ["Control", "Shift", "ArrowLeft"]);
        assert_eq!(chord("Control++"), ["Control", "+"]);
        assert_eq!(chord("+"), ["+"]);
        assert_eq!(chord("Shift++"), ["Shift", "+"]);
        assert_eq!(chord("Control+"), ["Control", ""], "a dangling separator names no key");
        assert_eq!(chord("Ñ+ñ"), ["Ñ", "ñ"]);
    }

    #[test]
    fn named_keys_carry_what_shortcuts_and_editing_match_on() {
        let enter = key("Enter", false).unwrap();
        assert_eq!((enter.key.as_str(), enter.code.as_str(), enter.key_code, enter.text.as_str()), ("Enter", "Enter", 13, "\r"));
        let left = key("ArrowLeft", false).unwrap();
        assert_eq!((left.key_code, left.text.as_str()), (37, ""));
        let space = key("Space", false).unwrap();
        assert_eq!((space.key.as_str(), space.code.as_str(), space.text.as_str()), (" ", "Space", " "));
        assert_eq!(key(" ", false), key("Space", false));
        assert_eq!(key("F1", false).unwrap().key_code, 112);
        assert_eq!(key("F12", false).unwrap().key_code, 123);
        assert_eq!(key("F25", false), None);
        assert_eq!(key("Delete", false).unwrap().key_code, 46);
    }

    #[test]
    fn modifiers_report_their_bit_side_and_generic_key_name() {
        for (name, bit, location) in [
            ("Shift", SHIFT, 1),
            ("ShiftRight", SHIFT, 2),
            ("Control", CONTROL, 1),
            ("ControlOrMeta", CONTROL, 1),
            ("Alt", ALT, 1),
            ("AltRight", ALT, 2),
            ("Meta", META, 1),
        ] {
            let resolved = key(name, false).unwrap();
            assert_eq!((resolved.modifier, resolved.location), (bit, location), "{name}");
            assert!(resolved.text.is_empty());
            assert!(["Shift", "Control", "Alt", "Meta"].contains(&resolved.key.as_str()), "{name}");
        }
        assert_eq!(key("ControlRight", false).unwrap().code, "ControlRight");
    }

    #[test]
    fn characters_and_positions_resolve_to_the_same_physical_key() {
        let lower = key("a", false).unwrap();
        assert_eq!((lower.key.as_str(), lower.code.as_str(), lower.key_code, lower.text.as_str()), ("a", "KeyA", 65, "a"));
        let upper = key("A", false).unwrap();
        assert_eq!((upper.key.as_str(), upper.code.as_str(), upper.key_code), ("A", "KeyA", 65));
        assert_eq!(key("KeyA", false).unwrap(), lower);
        assert_eq!(key("KeyA", true).unwrap(), upper);
        assert_eq!(key("Keya", false), None);
        let five = key("5", false).unwrap();
        assert_eq!((five.code.as_str(), five.key_code), ("Digit5", 53));
        let percent = key("%", false).unwrap();
        assert_eq!((percent.code.as_str(), percent.key_code, percent.text.as_str()), ("Digit5", 53, "%"));
        assert_eq!(key("Digit5", true).unwrap(), percent);
        let plus = key("+", false).unwrap();
        assert_eq!((plus.code.as_str(), plus.key_code), ("Equal", 187));
        assert_eq!(key("Equal", true).unwrap(), plus);
        assert_eq!(key("Slash", false).unwrap().text, "/");
        assert_eq!(key("?", false).unwrap().code, "Slash");
        // With Shift held, a key named by its plain character types the shifted one.
        assert_eq!(key("a", true).unwrap(), upper);
        assert_eq!(key("5", true).unwrap(), percent);
        assert_eq!(key("=", true).unwrap(), plus);
        assert_eq!(key("A", true).unwrap(), upper);
        assert_eq!(key("%", true).unwrap(), percent);
    }

    #[test]
    fn a_character_off_the_layout_types_itself_and_unknown_names_are_refused() {
        for character in ["ñ", "é", "🧠", "ß"] {
            let resolved = key(character, false).unwrap();
            assert_eq!((resolved.text.as_str(), resolved.code.as_str(), resolved.key_code), (character, "", 0));
        }
        for unknown in ["", "Entr", "enter", "Control+A", "ab", "\n", "F0"] {
            assert_eq!(key(unknown, false), None, "{unknown:?}");
        }
    }
}
