// Package repository reads a project working tree and its Git history for the
// Desktop surfaces. It never writes to the repository.
package repository

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

// Limits bounds the work and memory a repository read may consume.
type Limits struct {
	MaxEntries   int
	MaxFileBytes int64
	Timeout      time.Duration
}

// Compact returns limits suitable for an interactive view.
func Compact() Limits {
	return Limits{MaxEntries: 5000, MaxFileBytes: 2 << 20, Timeout: 10 * time.Second}
}

var (
	// ErrNotRepository means the root has no usable Git repository.
	ErrNotRepository = errors.New("repository: not a git repository")
	// ErrOutsideRoot rejects a path that escapes the project root.
	ErrOutsideRoot = errors.New("repository: path outside root")
	// ErrTooLarge rejects a file above the limit.
	ErrTooLarge = errors.New("repository: file exceeds limit")
)

// Repo is a read-only handle on one project root.
type Repo struct {
	root   string
	limits Limits
}

// Open validates root as a directory inside a Git worktree.
func Open(root string, limits Limits) (*Repo, error) {
	abs, err := filepath.Abs(strings.TrimSpace(root))
	if err != nil || abs == "" {
		return nil, fmt.Errorf("%w: %v", ErrNotRepository, err)
	}
	info, err := os.Stat(abs)
	if err != nil || !info.IsDir() {
		return nil, fmt.Errorf("%w: %s", ErrNotRepository, abs)
	}
	if limits.MaxEntries <= 0 {
		limits = Compact()
	}
	repo := &Repo{root: abs, limits: limits}
	ctx, cancel := context.WithTimeout(context.Background(), limits.Timeout)
	defer cancel()
	if _, err := repo.git(ctx, "rev-parse", "--show-toplevel"); err != nil {
		return nil, fmt.Errorf("%w: %v", ErrNotRepository, err)
	}
	return repo, nil
}

// Root returns the absolute project root.
func (r *Repo) Root() string { return r.root }

// StatusEntry is one changed path.
type StatusEntry struct {
	Path      string `json:"path"`
	Code      string `json:"code"`
	Staged    bool   `json:"staged"`
	Untracked bool   `json:"untracked"`
}

// Status is the working tree summary.
type Status struct {
	Branch   string        `json:"branch"`
	Upstream string        `json:"upstream,omitempty"`
	Ahead    int           `json:"ahead"`
	Behind   int           `json:"behind"`
	Entries  []StatusEntry `json:"entries"`
}

// Status reads porcelain v2 status without modifying the index.
func (r *Repo) Status(ctx context.Context) (Status, error) {
	var out Status
	branch, err := r.git(ctx, "symbolic-ref", "--quiet", "--short", "HEAD")
	if err == nil {
		out.Branch = strings.TrimSpace(branch)
	}
	if upstream, err := r.git(ctx, "rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"); err == nil {
		out.Upstream = strings.TrimSpace(upstream)
		if counts, err := r.git(ctx, "rev-list", "--left-right", "--count", "HEAD...@{upstream}"); err == nil {
			fields := strings.Fields(counts)
			if len(fields) == 2 {
				out.Ahead, _ = strconv.Atoi(fields[0])
				out.Behind, _ = strconv.Atoi(fields[1])
			}
		}
	}
	raw, err := r.git(ctx, "status", "--porcelain=v2", "-z", "--untracked-files=all")
	if err != nil {
		return out, err
	}
	for _, record := range strings.Split(raw, "\x00") {
		if record == "" {
			continue
		}
		entry := StatusEntry{Untracked: strings.HasPrefix(record, "? ")}
		switch record[0] {
		case '1', '2':
			fields := strings.Fields(record)
			if len(fields) >= 9 {
				xy := fields[1]
				entry.Staged = xy[0] != '.'
				entry.Code = xy
				entry.Path = fields[len(fields)-1]
				if record[0] == '2' && len(fields) >= 10 {
					entry.Path = fields[8]
				}
			}
		case 'u':
			fields := strings.Fields(record)
			if len(fields) >= 11 {
				entry.Code = fields[1]
				entry.Path = fields[10]
			}
		case '?':
			entry.Code = "??"
			entry.Path = strings.TrimSpace(strings.TrimPrefix(record, "? "))
		case '!':
			continue
		}
		if entry.Path != "" {
			out.Entries = append(out.Entries, entry)
			if len(out.Entries) >= r.limits.MaxEntries {
				break
			}
		}
	}
	return out, nil
}

// Scope selects what a diff compares.
type Scope int

const (
	// ScopeHead compares HEAD with the working tree.
	ScopeHead Scope = iota
	// ScopeStaged compares HEAD with the index.
	ScopeStaged
)

// Diff returns a unified patch, bounded by MaxEntries files worth of output.
func (r *Repo) Diff(ctx context.Context, scope Scope) (string, error) {
	args := []string{"diff", "--no-color", "--no-ext-diff", "--unified=3"}
	if scope == ScopeStaged {
		args = append(args, "--cached")
	}
	args = append(args, "--")
	return r.git(ctx, args...)
}

// DiffFile returns a patch for one path.
func (r *Repo) DiffFile(ctx context.Context, rel string, scope Scope) (string, error) {
	clean, err := r.cleanRel(rel)
	if err != nil {
		return "", err
	}
	args := []string{"diff", "--no-color", "--no-ext-diff", "--unified=3"}
	if scope == ScopeStaged {
		args = append(args, "--cached")
	}
	args = append(args, "--", clean)
	return r.git(ctx, args...)
}

// Commit is a recent history entry.
type Commit struct {
	Hash    string `json:"hash"`
	Author  string `json:"author"`
	WhenMS  int64  `json:"when_ms"`
	Subject string `json:"subject"`
}

// Activity returns recent commits, newest first.
func (r *Repo) Activity(ctx context.Context, limit int) ([]Commit, error) {
	if limit <= 0 || limit > 200 {
		limit = 30
	}
	raw, err := r.git(ctx, "log", "-n", strconv.Itoa(limit),
		"--pretty=format:%H%x1f%an%x1f%at%x1f%s%x1e")
	if err != nil {
		return nil, err
	}
	var out []Commit
	for _, record := range strings.Split(raw, "\x1e") {
		record = strings.TrimSpace(record)
		if record == "" {
			continue
		}
		fields := strings.Split(record, "\x1f")
		if len(fields) != 4 {
			continue
		}
		seconds, _ := strconv.ParseInt(fields[2], 10, 64)
		out = append(out, Commit{
			Hash: fields[0], Author: fields[1],
			WhenMS: seconds * 1000, Subject: fields[3],
		})
	}
	return out, nil
}

// Node is one entry of the bounded project tree.
type Node struct {
	Path  string `json:"path"`
	Name  string `json:"name"`
	Dir   bool   `json:"dir"`
	Depth int    `json:"depth"`
}

var skippedDirs = map[string]bool{
	".git": true, "node_modules": true, "target": true, "dist": true,
	".venv": true, "vendor": true, ".next": true, ".cache": true,
}

// Tree walks the project root breadth-first, bounded by MaxEntries.
func (r *Repo) Tree(ctx context.Context) ([]Node, error) {
	root := r.root
	queue := []Node{{Path: "", Dir: true, Depth: -1}}
	var out []Node
	for len(queue) > 0 {
		if err := ctx.Err(); err != nil {
			return out, err
		}
		current := queue[0]
		queue = queue[1:]
		dir := filepath.Join(root, filepath.FromSlash(current.Path))
		entries, err := os.ReadDir(dir)
		if err != nil {
			continue
		}
		for _, entry := range entries {
			name := entry.Name()
			if entry.IsDir() && skippedDirs[name] {
				continue
			}
			if strings.HasPrefix(name, ".") && name != ".github" && name != ".env.example" {
				continue
			}
			rel := name
			if current.Path != "" {
				rel = current.Path + "/" + name
			}
			node := Node{Path: rel, Name: name, Dir: entry.IsDir(), Depth: current.Depth + 1}
			out = append(out, node)
			if node.Dir {
				queue = append(queue, node)
			}
			if len(out) >= r.limits.MaxEntries {
				return out, nil
			}
		}
	}
	return out, nil
}

// Read returns a file's bytes after resolving it inside the root.
func (r *Repo) Read(rel string) ([]byte, error) {
	clean, err := r.cleanRel(rel)
	if err != nil {
		return nil, err
	}
	path := filepath.Join(r.root, filepath.FromSlash(clean))
	info, err := os.Stat(path)
	if err != nil {
		return nil, err
	}
	if info.IsDir() {
		return nil, fmt.Errorf("repository: %s is a directory", clean)
	}
	if info.Size() > r.limits.MaxFileBytes {
		return nil, fmt.Errorf("%w: %s", ErrTooLarge, clean)
	}
	return os.ReadFile(path)
}

func (r *Repo) cleanRel(rel string) (string, error) {
	rel = strings.TrimSpace(rel)
	if rel == "" {
		return "", fmt.Errorf("%w: empty path", ErrOutsideRoot)
	}
	clean := filepath.ToSlash(filepath.Clean(filepath.FromSlash(rel)))
	if clean == "." || clean == ".." || strings.HasPrefix(clean, "../") || filepath.IsAbs(clean) {
		return "", ErrOutsideRoot
	}
	resolved := filepath.Join(r.root, filepath.FromSlash(clean))
	within, err := filepath.Rel(r.root, resolved)
	if err != nil || within == ".." || strings.HasPrefix(within, ".."+string(filepath.Separator)) {
		return "", ErrOutsideRoot
	}
	return clean, nil
}

func (r *Repo) git(ctx context.Context, args ...string) (string, error) {
	if _, ok := ctx.Deadline(); !ok {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeout(ctx, r.limits.Timeout)
		defer cancel()
	}
	cmd := exec.CommandContext(ctx, "git", append([]string{"-C", r.root, "--no-pager"}, args...)...)
	cmd.Env = append(os.Environ(), "GIT_OPTIONAL_LOCKS=0", "GIT_TERMINAL_PROMPT=0", "LC_ALL=C")
	out, err := cmd.Output()
	if err != nil {
		var exit *exec.ExitError
		if errors.As(err, &exit) {
			message := strings.TrimSpace(string(exit.Stderr))
			if message == "" {
				message = exit.Error()
			}
			return "", fmt.Errorf("repository: git %s: %s", strings.Join(args, " "), message)
		}
		return "", fmt.Errorf("repository: git: %w", err)
	}
	return string(out), nil
}
