//go:build linux

package remotescreen

import (
	"context"
	"fmt"
	"os/exec"
	"strconv"
	"time"
)

const xdotoolTimeout = 2 * time.Second

// xdotoolInput is the Linux/X11 backend. It invokes xdotool directly with an
// argv slice (never a shell) and fails explicitly when the tool is absent.
type xdotoolInput struct {
	path string
}

func newPlatformInput() (Input, error) {
	path, err := exec.LookPath("xdotool")
	if err != nil {
		return nil, fmt.Errorf("%w: xdotool not found", ErrInputUnavailable)
	}
	return &xdotoolInput{path: path}, nil
}

func (x *xdotoolInput) Pointer(kind PointerKind, px, py int, button Button) error {
	args := []string{"mousemove", strconv.Itoa(px), strconv.Itoa(py)}
	switch kind {
	case PointerMove:
		if button != ButtonNone {
			return fmt.Errorf("%w: move with button %q", ErrInvalidInput, button)
		}
	case PointerDown, PointerUp:
		number, err := xdotoolButton(button)
		if err != nil {
			return err
		}
		if kind == PointerDown {
			args = append(args, "mousedown", strconv.Itoa(number))
		} else {
			args = append(args, "mouseup", strconv.Itoa(number))
		}
	default:
		return fmt.Errorf("%w: pointer kind %q", ErrInvalidInput, kind)
	}
	return x.run(args...)
}

func (x *xdotoolInput) Key(code KeyCode, down bool) error {
	keysym, ok := linuxKeyMap[code]
	if !ok {
		return fmt.Errorf("%w: %q", ErrUnsupportedKey, code)
	}
	action := "keydown"
	if !down {
		action = "keyup"
	}
	return x.run(action, keysym)
}

func (x *xdotoolInput) Wheel(dx, dy int) error {
	if absInt(dx) > MaxWheelNotches || absInt(dy) > MaxWheelNotches {
		return fmt.Errorf("%w: wheel (%d,%d)", ErrInvalidInput, dx, dy)
	}
	if dy != 0 {
		button := "4"
		if dy < 0 {
			button = "5"
		}
		if err := x.run("click", "--repeat", strconv.Itoa(absInt(dy)), button); err != nil {
			return fmt.Errorf("%w: vertical wheel: %v", ErrInputUnavailable, err)
		}
	}
	if dx != 0 {
		button := "7"
		if dx < 0 {
			button = "6"
		}
		if err := x.run("click", "--repeat", strconv.Itoa(absInt(dx)), button); err != nil {
			return fmt.Errorf("%w: horizontal wheel: %v", ErrInputUnavailable, err)
		}
	}
	return nil
}

func (x *xdotoolInput) run(args ...string) error {
	ctx, cancel := context.WithTimeout(context.Background(), xdotoolTimeout)
	defer cancel()
	cmd := exec.CommandContext(ctx, x.path, args...)
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("%w: xdotool %v: %v (%s)", ErrInputUnavailable, args, err, out)
	}
	return nil
}

func xdotoolButton(button Button) (int, error) {
	switch button {
	case ButtonLeft:
		return 1, nil
	case ButtonMiddle:
		return 2, nil
	case ButtonRight:
		return 3, nil
	default:
		return 0, fmt.Errorf("%w: button %q", ErrInvalidInput, button)
	}
}

var linuxKeyMap = func() map[KeyCode]string {
	m := make(map[KeyCode]string)
	for c := 'a'; c <= 'z'; c++ {
		m[KeyCode("Key"+string(c-'a'+'A'))] = string(c)
	}
	for c := '0'; c <= '9'; c++ {
		m[KeyCode("Digit"+string(c))] = string(c)
		m[KeyCode("Numpad"+string(c))] = "KP_" + string(c)
	}
	for i := 1; i <= 12; i++ {
		m[KeyCode("F"+strconv.Itoa(i))] = "F" + strconv.Itoa(i)
	}
	for code, keysym := range map[KeyCode]string{
		KeyEnter:          "Return",
		KeyEscape:         "Escape",
		KeyTab:            "Tab",
		KeySpace:          "space",
		KeyBackspace:      "BackSpace",
		KeyDelete:         "Delete",
		KeyInsert:         "Insert",
		KeyHome:           "Home",
		KeyEnd:            "End",
		KeyPageUp:         "Page_Up",
		KeyPageDown:       "Page_Down",
		KeyArrowUp:        "Up",
		KeyArrowDown:      "Down",
		KeyArrowLeft:      "Left",
		KeyArrowRight:     "Right",
		KeyMinus:          "minus",
		KeyEqual:          "equal",
		KeyBracketLeft:    "bracketleft",
		KeyBracketRight:   "bracketright",
		KeyBackslash:      "backslash",
		KeySemicolon:      "semicolon",
		KeyQuote:          "apostrophe",
		KeyBackquote:      "grave",
		KeyComma:          "comma",
		KeyPeriod:         "period",
		KeySlash:          "slash",
		KeyCapsLock:       "Caps_Lock",
		KeyShiftLeft:      "Shift_L",
		KeyShiftRight:     "Shift_R",
		KeyControlLeft:    "Control_L",
		KeyControlRight:   "Control_R",
		KeyAltLeft:        "Alt_L",
		KeyAltRight:       "Alt_R",
		KeyMetaLeft:       "Super_L",
		KeyMetaRight:      "Super_R",
		KeyNumpadAdd:      "KP_Add",
		KeyNumpadSubtract: "KP_Subtract",
		KeyNumpadMultiply: "KP_Multiply",
		KeyNumpadDivide:   "KP_Divide",
		KeyNumpadDecimal:  "KP_Decimal",
		KeyNumpadEnter:    "KP_Enter",
	} {
		m[code] = keysym
	}
	return m
}()
