// Package repository renders the read-only Files and Diff surfaces.
package repository

import (
	"fmt"
	"path"
	"strings"

	"github.com/egoist/mygo/ui"
	"github.com/egoist/mygo/ui/editor"
	repo "github.com/suj1e/ukk/apps/desktop/internal/repository"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/kit"
)

// FilesState is the Files pane.
type FilesState struct {
	Repo     *repo.Repo
	Nodes    []repo.Node
	Status   repo.Status
	Expanded map[string]bool
	Filter   string
	Selected string
	Content  string
	Binary   bool
	Err      string
	Loading  bool
	Editor   *editor.State
	Scroll   ui.ScrollState
}

// NewFiles returns an empty Files pane for a project root.
func NewFiles(target *repo.Repo) *FilesState {
	return &FilesState{Repo: target, Expanded: map[string]bool{}}
}

// SetContent shows a file. The read happens on the caller's goroutine.
func (s *FilesState) SetContent(rel string, data []byte, err error) {
	s.Selected = rel
	s.Err = ""
	s.Binary = false
	if err != nil {
		s.Err = err.Error()
		s.Content = ""
		s.Editor = nil
		return
	}
	if strings.ContainsRune(string(data[:min(len(data), 4096)]), 0) {
		s.Binary = true
		s.Content = ""
		s.Editor = nil
		return
	}
	s.Content = string(data)
	state := &editor.State{Buffer: editor.NewBuffer(s.Content)}
	if language := editor.LanguageFor(path.Base(rel)); language != nil {
		state.Buffer.SetLanguage(language)
	}
	state.ReadOnly = true
	s.Editor = state
}

// Toggle expands or collapses a directory.
func (s *FilesState) Toggle(node repo.Node) {
	if !node.Dir {
		return
	}
	s.Expanded[node.Path] = !s.Expanded[node.Path]
}

func (s *FilesState) visible() []repo.Node {
	if s.Filter != "" {
		query := strings.ToLower(s.Filter)
		var out []repo.Node
		for _, node := range s.Nodes {
			if strings.Contains(strings.ToLower(node.Path), query) {
				out = append(out, node)
			}
			if len(out) >= 400 {
				break
			}
		}
		return out
	}
	var out []repo.Node
	for _, node := range s.Nodes {
		if !s.ancestorsOpen(node.Path) {
			continue
		}
		out = append(out, node)
	}
	return out
}

func (s *FilesState) ancestorsOpen(rel string) bool {
	dir := path.Dir(rel)
	for dir != "." && dir != "/" {
		if !s.Expanded[dir] {
			return false
		}
		dir = path.Dir(dir)
	}
	return true
}

// View renders the Files pane.
func View(c *ui.Context, s *FilesState, reload func(), openDiff func(), onSelect func(path string)) {
	theme := c.Theme()
	ui.Row(c).FillWidth().Height(34).PaddingX(10).Gap(8).AlignItems(ui.Center).
		Background(theme.Surface).BorderWidth(0, 0, 1, 0).BorderColor(theme.Border).
		Children(func() {
			branch := s.Status.Branch
			if branch == "" {
				branch = "detached"
			}
			ui.Text(c, branch).FontSize(11).FontWeight(600).NoWrap()
			kit.Meta(c, kit.Plural(len(s.Status.Entries), "change", "changes"))
			if s.Status.Ahead > 0 {
				kit.Pill(c, fmt.Sprintf("↑%d", s.Status.Ahead), "")
			}
			if s.Status.Behind > 0 {
				kit.Pill(c, fmt.Sprintf("↓%d", s.Status.Behind), "")
			}
			ui.Spacer(c)
			if s.Loading {
				ui.Spinner(c).Size(13, 13)
			}
			if openDiff != nil {
				kit.ToolbarButton(c, "Diff").OnClick(openDiff)
			}
			if reload != nil {
				kit.ToolbarButton(c, "Refresh").OnClick(reload)
			}
		})
	kit.ErrorLine(c, s.Err)

	ui.Split(c, new(float32(260)), func() {
		ui.Column(c).FillHeight().Children(func() {
			ui.Box(c).Padding(6, 8).Children(func() {
				ui.SearchField(c, &s.Filter).Placeholder("Filter files").FillWidth()
			})
			nodes := s.visible()
			ui.Scroll(c).Grow(1).Children(func() {
				ui.Column(c).Gap(1).PaddingX(4).Children(func() {
					for _, node := range nodes {
						row := node
						el := ui.Row(c).FillWidth().Gap(5).Padding(4, 6).Radius(5).Children(func() {
							indent := ui.Box(c).Width(float32(row.Depth) * 12)
							indent.Children(func() {})
							mark := "·"
							if row.Dir {
								if s.Expanded[row.Path] {
									mark = "▾"
								} else {
									mark = "▸"
								}
							}
							ui.Text(c, mark).FontSize(10).TextColor(theme.TextMuted)
							ui.Text(c, row.Name).FontSize(11).NoWrap().
								Ellipsis(row.Name).Grow(1)
						})
						if row.Path == s.Selected {
							el.Background(theme.Selection)
						}
						if el.Clicked() {
							if row.Dir {
								s.Toggle(row)
							} else if onSelect != nil {
								onSelect(row.Path)
							}
						}
					}
				})
			})
		})
	}, func() {
		ui.Column(c).FillHeight().Background(theme.Background).Children(func() {
			if s.Selected == "" {
				kit.Empty(c, "Select a file", "Files open read-only. Use Diff to review changes.", "")
				return
			}
			ui.Row(c).FillWidth().Padding(6, 10).Gap(8).AlignItems(ui.Center).Children(func() {
				kit.Mono(c, s.Selected)
				ui.Spacer(c)
			})
			if s.Binary {
				kit.Empty(c, "Binary file", "This file is not shown as text.", "")
				return
			}
			if s.Editor == nil {
				kit.Empty(c, "Nothing to show", s.Err, "")
				return
			}
			editor.View(c, s.Editor).Fill()
		})
	}).Fill().SplitReset(260)
}

// DiffState is the Diff pane.
type DiffState struct {
	Patch    string
	Files    []editor.FilePatch
	Selected int
	Diff     *editor.DiffState
	Err      string
	Stats    string
}

// SetPatch replaces the patch and selects its first file.
func (s *DiffState) SetPatch(patch string) {
	s.Patch = patch
	s.Err = ""
	s.Files = nil
	s.Diff = nil
	s.Selected = -1
	files, err := editor.ParsePatch(patch)
	if err != nil {
		s.Err = "Could not parse the diff: " + err.Error()
		return
	}
	s.Files = files
	if len(files) > 0 {
		s.Select(0)
	}
}

// Select shows one file's change.
func (s *DiffState) Select(i int) {
	if i < 0 || i >= len(s.Files) {
		return
	}
	s.Selected = i
	state := &editor.DiffState{
		Layout:     editor.DiffSplit,
		Inline:     editor.DiffInlineWordAlt,
		Indicators: editor.DiffBars,
		Theme:      nil,
	}
	state.SetPatch(s.Files[i])
	added, deleted := state.Stats()
	s.Stats = fmt.Sprintf("+%d −%d", added, deleted)
	s.Diff = state
}

// DiffView renders the Diff pane.
func DiffView(c *ui.Context, s *DiffState, reload func()) {
	theme := c.Theme()
	ui.Row(c).FillWidth().Height(34).PaddingX(10).Gap(8).AlignItems(ui.Center).
		Background(theme.Surface).BorderWidth(0, 0, 1, 0).BorderColor(theme.Border).
		Children(func() {
			ui.Text(c, "Changes").FontSize(12).FontWeight(600)
			kit.Meta(c, kit.Plural(len(s.Files), "file", "files"))
			if s.Stats != "" {
				kit.Meta(c, s.Stats)
			}
			ui.Spacer(c)
			if reload != nil {
				kit.ToolbarButton(c, "Refresh").OnClick(reload)
			}
		})
	kit.ErrorLine(c, s.Err)
	if len(s.Files) == 0 {
		kit.Empty(c, "No changes", "The working tree matches HEAD.", "")
		return
	}
	ui.Row(c).Grow(1).Children(func() {
		ui.Column(c).Width(230).FillHeight().BorderWidth(0, 1, 0, 0).
			BorderColor(theme.Border).Children(func() {
			ui.Scroll(c).Grow(1).Children(func() {
				ui.Column(c).Gap(1).Padding(4).Children(func() {
					for i, file := range s.Files {
						name := file.NewName
						if name == "" {
							name = file.OldName
						}
						row := ui.Row(c).FillWidth().Padding(4, 7).Radius(5)
						if i == s.Selected {
							row.Background(theme.Selection)
						}
						row.Children(func() {
							ui.Text(c, name).FontSize(11).NoWrap().Ellipsis(name)
						})
						if row.Clicked() {
							s.Select(i)
						}
					}
				})
			})
		})
		ui.Column(c).Grow(1).FillHeight().Children(func() {
			if s.Diff == nil {
				kit.Empty(c, "Select a file", "The change will show here.", "")
				return
			}
			editor.DiffView(c, s.Diff).Fill()
		})
	})
}
