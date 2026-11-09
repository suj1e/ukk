// Package kit holds the shared visual language and small composed widgets of
// the Desktop. It owns no state and never touches a terminal or Agent.
package kit

import (
	"fmt"
	"strings"

	"github.com/egoist/mygo/ui"
)

// Theme returns a copy of the system theme with ukk's colors and metrics.
func Theme(dark bool) *ui.Theme {
	var theme *ui.Theme
	if dark {
		theme = ui.DarkTheme()
		theme.Background = ui.Hex("#0E1116")
		theme.Surface = ui.Hex("#151A21")
		theme.SurfaceHover = ui.Hex("#1C232C")
		theme.SurfacePressed = ui.Hex("#232C37")
		theme.Border = ui.Hex("#232C37")
		theme.Text = ui.Hex("#E6EDF3")
		theme.TextMuted = ui.Hex("#8B98A9")
		theme.Accent = ui.Hex("#5AA9FF")
		theme.AccentHover = ui.Hex("#74B8FF")
		theme.AccentPressed = ui.Hex("#4795EA")
		theme.AccentText = ui.Hex("#06121F")
		theme.Selection = ui.Hex("#1E3A5F")
		theme.Scrollbar = ui.Hex("#33404F")
		theme.Inverse = ui.Hex("#E6EDF3")
		theme.InverseText = ui.Hex("#0E1116")
		theme.Success = ui.Hex("#4CC38A")
		theme.Warning = ui.Hex("#E3B341")
		theme.Danger = ui.Hex("#F85149")
	} else {
		theme = ui.LightTheme()
		theme.Background = ui.Hex("#F6F6F8")
		theme.Surface = ui.Hex("#FFFFFF")
		theme.SurfaceHover = ui.Hex("#F0F0F4")
		theme.SurfacePressed = ui.Hex("#E7E7EE")
		theme.Border = ui.Hex("#E1E2E9")
		theme.Text = ui.Hex("#242730")
		theme.TextMuted = ui.Hex("#6B7180")
		theme.Accent = ui.Hex("#5F66D9")
		theme.AccentHover = ui.Hex("#6E74E8")
		theme.AccentPressed = ui.Hex("#535AC9")
		theme.AccentText = ui.Hex("#FFFFFF")
		theme.Selection = ui.Hex("#D7D9FA")
		theme.Scrollbar = ui.Hex("#C5C7D2")
		theme.Inverse = ui.Hex("#242730")
		theme.InverseText = ui.Hex("#F8F9FB")
	}
	theme.Radius = 7
	theme.Spacing = 4
	theme.FontSize = 13
	theme.Focus = theme.Accent
	return theme
}

// PageTitle is the primary heading of a surface.
func PageTitle(c *ui.Context, title string) ui.Element {
	return ui.Text(c, title).FontSize(17).FontWeight(600).Role(ui.RoleHeading).Level(1)
}

// SectionLabel labels a group without competing with content.
func SectionLabel(c *ui.Context, label string) ui.Element {
	return ui.Text(c, strings.ToUpper(label)).FontSize(10).FontWeight(600).
		LetterSpacing(0.6).TextColor(c.Theme().TextMuted)
}

// Meta is secondary text.
func Meta(c *ui.Context, text string) ui.Element {
	return ui.Text(c, text).FontSize(11).TextColor(c.Theme().TextMuted)
}

// Mono is fixed text such as a path or an identifier.
func Mono(c *ui.Context, text string) ui.Element {
	return ui.Text(c, text).FontSize(11).TextColor(c.Theme().TextMuted).NoWrap().Ellipsis(text)
}

// Pill is a compact status badge. kind is "", "ok", "warn" or "danger".
func Pill(c *ui.Context, label, kind string) ui.Element {
	theme := c.Theme()
	color := theme.TextMuted
	switch kind {
	case "ok":
		color = theme.Success
	case "warn":
		color = theme.Warning
	case "danger":
		color = theme.Danger
	case "accent":
		color = theme.Accent
	}
	return ui.Text(c, label).FontSize(10).FontWeight(600).
		TextColor(color).PaddingX(6).PaddingY(2).
		Border(1, color).Radius(999)
}

// Dot is a small status mark.
func Dot(c *ui.Context, color ui.Color) ui.Element {
	return ui.Box(c).Size(7, 7).Radius(999).Background(color)
}

// ToolbarButton is a compact push button for a toolbar.
func ToolbarButton(c *ui.Context, label string) ui.Element {
	return ui.Button(c, label).PaddingX(9).PaddingY(4).FontSize(12)
}

// PrimaryAction is the one emphasized action of a surface.
func PrimaryAction(c *ui.Context, label string) ui.Element {
	return ui.PrimaryButton(c, label).PaddingX(12).PaddingY(5).FontSize(12)
}

// Empty explains why a surface has nothing to show and what the user can do.
// It reports its action's click.
func Empty(c *ui.Context, title, body, action string) bool {
	clicked := false
	ui.Column(c).Fill().Center().Gap(8).Padding(24).Children(func() {
		ui.Text(c, title).FontSize(15).FontWeight(600).TextAlign(ui.Center)
		ui.Text(c, body).FontSize(12).TextColor(c.Theme().TextMuted).
			TextAlign(ui.Center).MaxWidth(420)
		if action != "" {
			clicked = ui.PrimaryButton(c, action).Clicked()
		}
	})
	return clicked
}

// KeyValue is a label and its value on one row.
func KeyValue(c *ui.Context, label, value string) {
	ui.Row(c).WidthPercent(100).Gap(10).Children(func() {
		ui.Text(c, label).FontSize(11).Width(110).TextColor(c.Theme().TextMuted)
		ui.Text(c, value).FontSize(11).Grow(1).Wrap()
	})
}

// ErrorLine shows an error without claiming success.
func ErrorLine(c *ui.Context, message string) {
	if message == "" {
		return
	}
	ui.Text(c, message).FontSize(11).TextColor(c.Theme().Danger).Wrap().
		Role(ui.RoleAlert)
}

// Banner is an inline notice with an optional action.
func Banner(c *ui.Context, tone, message, action string) bool {
	theme := c.Theme()
	color := theme.TextMuted
	switch tone {
	case "danger":
		color = theme.Danger
	case "warn":
		color = theme.Warning
	case "ok":
		color = theme.Success
	}
	clicked := false
	ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).
		Padding(8, 10).Radius(theme.Radius).
		Background(theme.Surface).Border(1, theme.Border).Children(func() {
		ui.Box(c).Width(3).Height(16).Radius(2).Background(color)
		ui.Text(c, message).FontSize(11).Grow(1).Wrap().TextColor(theme.Text)
		if action != "" {
			clicked = ui.Button(c, action).PaddingX(8).PaddingY(3).FontSize(11).Clicked()
		}
	})
	return clicked
}

// ShortID shortens an identifier for display.
func ShortID(id string) string {
	if len(id) <= 8 {
		return id
	}
	return id[:8]
}

// Plural formats a count with a noun.
func Plural(n int, one, many string) string {
	if n == 1 {
		return fmt.Sprintf("%d %s", n, one)
	}
	return fmt.Sprintf("%d %s", n, many)
}

// FormatDuration is a compact elapsed time.
func FormatDuration(ms int64) string {
	if ms <= 0 {
		return "0s"
	}
	seconds := ms / 1000
	switch {
	case seconds < 60:
		return fmt.Sprintf("%ds", seconds)
	case seconds < 3600:
		return fmt.Sprintf("%dm", seconds/60)
	default:
		return fmt.Sprintf("%dh%02dm", seconds/3600, (seconds%3600)/60)
	}
}
