//go:build windows

package remotescreen

import (
	"fmt"
	"strconv"
	"unsafe"

	"golang.org/x/sys/windows"
)

// Windows SendInput structures. INPUT contains a union whose largest member
// is MOUSEINPUT, so winInput reproduces the layout for both 32-bit and 64-bit
// Windows without hard-coding a size.
type winMouseInput struct {
	DX        int32
	DY        int32
	MouseData uint32
	Flags     uint32
	Time      uint32
	ExtraInfo uintptr
}

type winKeyboardInput struct {
	VK        uint16
	Scan      uint16
	Flags     uint32
	Time      uint32
	ExtraInfo uintptr
}

type winInput struct {
	Type uint32
	Data winMouseInput
}

const (
	inputMouse = 0
	inputKey   = 1

	mouseMove        = 0x0001
	mouseLeftDown    = 0x0002
	mouseLeftUp      = 0x0004
	mouseRightDown   = 0x0008
	mouseRightUp     = 0x0010
	mouseMiddleDown  = 0x0020
	mouseMiddleUp    = 0x0040
	mouseWheel       = 0x0800
	mouseHWheel      = 0x1000
	mouseAbsolute    = 0x8000
	mouseVirtualDesk = 0x4000

	keyEventExtended = 0x0001
	keyEventUp       = 0x0002

	wheelDelta = 120

	smXVirtualScreen  = 76
	smYVirtualScreen  = 77
	smCXVirtualScreen = 78
	smCYVirtualScreen = 79
)

var (
	user32               = windows.NewLazySystemDLL("user32.dll")
	procSendInput        = user32.NewProc("SendInput")
	procGetSystemMetrics = user32.NewProc("GetSystemMetrics")
)

type windowsInput struct{}

func newPlatformInput() (Input, error) {
	return &windowsInput{}, nil
}

func (w *windowsInput) Pointer(kind PointerKind, x, y int, button Button) error {
	ox, oy, width, height, err := virtualScreen()
	if err != nil {
		return err
	}
	if x < ox || y < oy || x >= ox+width || y >= oy+height {
		return fmt.Errorf("%w: (%d,%d) not in virtual screen", ErrPointOutOfBounds, x, y)
	}

	ax, ay := normalizeAbsolute(x, y, ox, oy, width, height)
	flags := uint32(mouseMove | mouseAbsolute | mouseVirtualDesk)

	switch kind {
	case PointerMove:
		if button != ButtonNone {
			return fmt.Errorf("%w: move with button %q", ErrInvalidInput, button)
		}
	case PointerDown:
		f, err := buttonFlag(button, true)
		if err != nil {
			return err
		}
		flags |= f
	case PointerUp:
		f, err := buttonFlag(button, false)
		if err != nil {
			return err
		}
		flags |= f
	default:
		return fmt.Errorf("%w: pointer kind %q", ErrInvalidInput, kind)
	}

	return sendMouse(int32(ax), int32(ay), 0, flags)
}

func (w *windowsInput) Key(code KeyCode, down bool) error {
	mapping, ok := windowsKeyMap[code]
	if !ok {
		return fmt.Errorf("%w: %q", ErrUnsupportedKey, code)
	}
	var kb winKeyboardInput
	kb.VK = mapping.vk
	kb.Flags = 0
	if mapping.extended {
		kb.Flags |= keyEventExtended
	}
	if !down {
		kb.Flags |= keyEventUp
	}
	in := winInput{Type: inputKey}
	*(*winKeyboardInput)(unsafe.Pointer(&in.Data)) = kb
	if err := sendInput(in); err != nil {
		return fmt.Errorf("%w: keyboard: %v", ErrInputUnavailable, err)
	}
	return nil
}

func (w *windowsInput) Wheel(dx, dy int) error {
	if absInt(dx) > MaxWheelNotches || absInt(dy) > MaxWheelNotches {
		return fmt.Errorf("%w: wheel (%d,%d)", ErrInvalidInput, dx, dy)
	}
	if dy != 0 {
		if err := sendMouse(0, 0, uint32(int32(dy*wheelDelta)), mouseWheel); err != nil {
			return fmt.Errorf("%w: vertical wheel: %v", ErrInputUnavailable, err)
		}
	}
	if dx != 0 {
		if err := sendMouse(0, 0, uint32(int32(dx*wheelDelta)), mouseHWheel); err != nil {
			return fmt.Errorf("%w: horizontal wheel: %v", ErrInputUnavailable, err)
		}
	}
	return nil
}

func sendMouse(dx, dy int32, data uint32, flags uint32) error {
	in := winInput{Type: inputMouse}
	*(*winMouseInput)(unsafe.Pointer(&in.Data)) = winMouseInput{
		DX:        dx,
		DY:        dy,
		MouseData: data,
		Flags:     flags,
	}
	if err := sendInput(in); err != nil {
		return fmt.Errorf("%w: mouse: %v", ErrInputUnavailable, err)
	}
	return nil
}

func sendInput(in winInput) error {
	n, _, callErr := procSendInput.Call(
		uintptr(1),
		uintptr(unsafe.Pointer(&in)),
		unsafe.Sizeof(in),
	)
	if n != 1 {
		if callErr != nil {
			return callErr
		}
		return fmt.Errorf("SendInput rejected event")
	}
	return nil
}

func virtualScreen() (left, top, width, height int, err error) {
	get := func(index int) (int, error) {
		v, _, callErr := procGetSystemMetrics.Call(uintptr(index))
		if v == 0 && callErr != nil {
			return 0, callErr
		}
		return int(int32(v)), nil
	}
	if left, err = get(smXVirtualScreen); err != nil {
		return
	}
	if top, err = get(smYVirtualScreen); err != nil {
		return
	}
	if width, err = get(smCXVirtualScreen); err != nil {
		return
	}
	if height, err = get(smCYVirtualScreen); err != nil {
		return
	}
	if width <= 1 || height <= 1 {
		err = fmt.Errorf("%w: virtual screen is %dx%d", ErrInputUnavailable, width, height)
	}
	return
}

func normalizeAbsolute(x, y, left, top, width, height int) (int, int) {
	ax, ay := 0, 0
	if width > 1 {
		ax = (x - left) * 65535 / (width - 1)
	}
	if height > 1 {
		ay = (y - top) * 65535 / (height - 1)
	}
	return ax, ay
}

func buttonFlag(button Button, down bool) (uint32, error) {
	switch button {
	case ButtonLeft:
		if down {
			return mouseLeftDown, nil
		}
		return mouseLeftUp, nil
	case ButtonRight:
		if down {
			return mouseRightDown, nil
		}
		return mouseRightUp, nil
	case ButtonMiddle:
		if down {
			return mouseMiddleDown, nil
		}
		return mouseMiddleUp, nil
	default:
		return 0, fmt.Errorf("%w: button %q", ErrInvalidInput, button)
	}
}

type windowsKey struct {
	vk       uint16
	extended bool
}

var windowsKeyMap = func() map[KeyCode]windowsKey {
	m := make(map[KeyCode]windowsKey)
	for c := 'A'; c <= 'Z'; c++ {
		m[KeyCode("Key"+string(c))] = windowsKey{vk: uint16(c)}
	}
	for c := '0'; c <= '9'; c++ {
		m[KeyCode("Digit"+string(c))] = windowsKey{vk: uint16(c)}
		m[KeyCode("Numpad"+string(c))] = windowsKey{vk: 0x60 + uint16(c-'0')}
	}
	for i := 1; i <= 12; i++ {
		m[KeyCode("F"+strconv.Itoa(i))] = windowsKey{vk: uint16(0x70 + i - 1)}
	}
	for code, key := range map[KeyCode]windowsKey{
		KeyEnter:          {vk: 0x0D},
		KeyEscape:         {vk: 0x1B},
		KeyTab:            {vk: 0x09},
		KeySpace:          {vk: 0x20},
		KeyBackspace:      {vk: 0x08},
		KeyDelete:         {vk: 0x2E, extended: true},
		KeyInsert:         {vk: 0x2D, extended: true},
		KeyHome:           {vk: 0x24, extended: true},
		KeyEnd:            {vk: 0x23, extended: true},
		KeyPageUp:         {vk: 0x21, extended: true},
		KeyPageDown:       {vk: 0x22, extended: true},
		KeyArrowUp:        {vk: 0x26, extended: true},
		KeyArrowDown:      {vk: 0x28, extended: true},
		KeyArrowLeft:      {vk: 0x25, extended: true},
		KeyArrowRight:     {vk: 0x27, extended: true},
		KeyMinus:          {vk: 0xBD},
		KeyEqual:          {vk: 0xBB},
		KeyBracketLeft:    {vk: 0xDB},
		KeyBracketRight:   {vk: 0xDD},
		KeyBackslash:      {vk: 0xDC},
		KeySemicolon:      {vk: 0xBA},
		KeyQuote:          {vk: 0xDE},
		KeyBackquote:      {vk: 0xC0},
		KeyComma:          {vk: 0xBC},
		KeyPeriod:         {vk: 0xBE},
		KeySlash:          {vk: 0xBF},
		KeyCapsLock:       {vk: 0x14},
		KeyShiftLeft:      {vk: 0xA0},
		KeyShiftRight:     {vk: 0xA1},
		KeyControlLeft:    {vk: 0xA2},
		KeyControlRight:   {vk: 0xA3},
		KeyAltLeft:        {vk: 0xA4},
		KeyAltRight:       {vk: 0xA5},
		KeyMetaLeft:       {vk: 0x5B, extended: true},
		KeyMetaRight:      {vk: 0x5C, extended: true},
		KeyNumpadMultiply: {vk: 0x6A},
		KeyNumpadAdd:      {vk: 0x6B},
		KeyNumpadSubtract: {vk: 0x6D},
		KeyNumpadDecimal:  {vk: 0x6E},
		KeyNumpadDivide:   {vk: 0x6F},
		KeyNumpadEnter:    {vk: 0x0D, extended: true},
	} {
		m[code] = key
	}
	return m
}()
