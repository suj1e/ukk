package remotescreen

import "strconv"

// Canonical key names. They are stable across platforms and are the only
// values accepted by GatedInput.
const (
	KeyEnter          KeyCode = "Enter"
	KeyEscape         KeyCode = "Escape"
	KeyTab            KeyCode = "Tab"
	KeySpace          KeyCode = "Space"
	KeyBackspace      KeyCode = "Backspace"
	KeyDelete         KeyCode = "Delete"
	KeyInsert         KeyCode = "Insert"
	KeyHome           KeyCode = "Home"
	KeyEnd            KeyCode = "End"
	KeyPageUp         KeyCode = "PageUp"
	KeyPageDown       KeyCode = "PageDown"
	KeyArrowUp        KeyCode = "ArrowUp"
	KeyArrowDown      KeyCode = "ArrowDown"
	KeyArrowLeft      KeyCode = "ArrowLeft"
	KeyArrowRight     KeyCode = "ArrowRight"
	KeyMinus          KeyCode = "Minus"
	KeyEqual          KeyCode = "Equal"
	KeyBracketLeft    KeyCode = "BracketLeft"
	KeyBracketRight   KeyCode = "BracketRight"
	KeyBackslash      KeyCode = "Backslash"
	KeySemicolon      KeyCode = "Semicolon"
	KeyQuote          KeyCode = "Quote"
	KeyBackquote      KeyCode = "Backquote"
	KeyComma          KeyCode = "Comma"
	KeyPeriod         KeyCode = "Period"
	KeySlash          KeyCode = "Slash"
	KeyCapsLock       KeyCode = "CapsLock"
	KeyShiftLeft      KeyCode = "ShiftLeft"
	KeyShiftRight     KeyCode = "ShiftRight"
	KeyControlLeft    KeyCode = "ControlLeft"
	KeyControlRight   KeyCode = "ControlRight"
	KeyAltLeft        KeyCode = "AltLeft"
	KeyAltRight       KeyCode = "AltRight"
	KeyMetaLeft       KeyCode = "MetaLeft"
	KeyMetaRight      KeyCode = "MetaRight"
	KeyNumpadAdd      KeyCode = "NumpadAdd"
	KeyNumpadSubtract KeyCode = "NumpadSubtract"
	KeyNumpadMultiply KeyCode = "NumpadMultiply"
	KeyNumpadDivide   KeyCode = "NumpadDivide"
	KeyNumpadDecimal  KeyCode = "NumpadDecimal"
	KeyNumpadEnter    KeyCode = "NumpadEnter"
)

var canonicalKeys = func() map[KeyCode]struct{} {
	keys := make(map[KeyCode]struct{})
	add := func(k KeyCode) { keys[k] = struct{}{} }
	add(KeyEnter)
	add(KeyEscape)
	add(KeyTab)
	add(KeySpace)
	add(KeyBackspace)
	add(KeyDelete)
	add(KeyInsert)
	add(KeyHome)
	add(KeyEnd)
	add(KeyPageUp)
	add(KeyPageDown)
	add(KeyArrowUp)
	add(KeyArrowDown)
	add(KeyArrowLeft)
	add(KeyArrowRight)
	add(KeyMinus)
	add(KeyEqual)
	add(KeyBracketLeft)
	add(KeyBracketRight)
	add(KeyBackslash)
	add(KeySemicolon)
	add(KeyQuote)
	add(KeyBackquote)
	add(KeyComma)
	add(KeyPeriod)
	add(KeySlash)
	add(KeyCapsLock)
	add(KeyShiftLeft)
	add(KeyShiftRight)
	add(KeyControlLeft)
	add(KeyControlRight)
	add(KeyAltLeft)
	add(KeyAltRight)
	add(KeyMetaLeft)
	add(KeyMetaRight)
	add(KeyNumpadAdd)
	add(KeyNumpadSubtract)
	add(KeyNumpadMultiply)
	add(KeyNumpadDivide)
	add(KeyNumpadDecimal)
	add(KeyNumpadEnter)
	for c := 'A'; c <= 'Z'; c++ {
		add(KeyCode("Key" + string(c)))
	}
	for c := '0'; c <= '9'; c++ {
		add(KeyCode("Digit" + string(c)))
		add(KeyCode("Numpad" + string(c)))
	}
	for i := 1; i <= 12; i++ {
		add(KeyCode("F" + strconv.Itoa(i)))
	}
	return keys
}()

func validKeyCode(code KeyCode) bool {
	_, ok := canonicalKeys[code]
	return ok
}
