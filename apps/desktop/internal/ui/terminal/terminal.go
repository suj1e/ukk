// Package terminal renders a herdr-core terminal through libghostty-vt. The
// terminal is a view: closing it drops the attachment and never stops the
// program.
package terminal

import (
	"fmt"

	mygoterm "github.com/egoist/mygo/plugins/terminal"
	"github.com/egoist/mygo/ui"
	"github.com/suj1e/ukk/apps/desktop/internal/hc"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/kit"
)

// State is one terminal pane.
type State struct {
	Address    string
	Label      string
	Observe    bool
	Connecting bool
	Err        string
	Exit       string
	Title      string
	OnTitle    func(string)
	OnExit     func(string)

	term *mygoterm.Terminal
}

// New returns a connecting terminal pane state.
func New(address, label string, observe bool) *State {
	return &State{Address: address, Label: label, Observe: observe, Connecting: true}
}

// Attach binds an attachment. It must run on the UI thread, as from
// Window.Update.
func (s *State) Attach(att *hc.Attachment) error {
	if s.term != nil {
		return nil
	}
	theme := mygoterm.LightTheme()
	dark := mygoterm.DarkTheme()
	term, err := mygoterm.New(mygoterm.Options{
		Conn:       att,
		Theme:      theme,
		DarkTheme:  dark,
		Cursor:     mygoterm.CursorBar,
		Scrollback: 20 << 20,
		Padding:    mygoterm.Padding{X: 14, Y: 12},
		OnTitle: func(title string) {
			if s.OnTitle != nil {
				s.OnTitle(title)
			}
		},
		OnExit: func(code int) {
			if s.OnExit != nil {
				s.OnExit(fmt.Sprintf("Program exited (%d)", code))
			}
		},
		ClipboardRead: mygoterm.ClipboardAsk,
	})
	if err != nil {
		s.Connecting = false
		s.Err = err.Error()
		_ = att.Close()
		return err
	}
	s.term = term
	s.Connecting = false
	s.Err = ""
	return nil
}

// Term returns the underlying terminal for live theming or commands.
func (s *State) Term() *mygoterm.Terminal { return s.term }

// Close drops the attachment. It never sends a stop or kill.
func (s *State) Close() {
	if s.term != nil {
		_ = s.term.Close()
		s.term = nil
	}
}

// View renders the terminal or its connection state.
func View(c *ui.Context, s *State) {
	if s.term == nil {
		switch {
		case s.Connecting:
			ui.Column(c).Fill().Center().Gap(8).Children(func() {
				ui.Spinner(c).Size(18, 18)
				kit.Meta(c, "Attaching read/write stream to "+s.Address)
			})
		default:
			kit.Empty(c, "Terminal not attached", fallback(s.Err, "The stream is closed. Reattach to continue; the program was not stopped."), "")
		}
		return
	}
	mygoterm.View(c, s.term).Fill().AutoFocus()
	if s.Exit != "" {
		ui.Row(c).FillWidth().Padding(4, 8).Background(c.Theme().Surface).Children(func() {
			kit.Meta(c, s.Exit)
		})
	}
}

func fallback(value, other string) string {
	if value != "" {
		return value
	}
	return other
}
