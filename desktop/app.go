package main

import (
	"log"
	"strings"
	"time"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"
	"github.com/egoist/mygo/ui/markdown"

	"github.com/Abelliuxl/Van-Goal/desktop/bridge"
)

// app is everything the window draws. The Rust core owns the conversation;
// this holds the last copy of it the core reported, plus what is purely the
// window's own: the draft, what is open, what is hovered.
type app struct {
	win *mygo.Window

	state    bridge.State
	settings bridge.Settings
	messages []bridge.Message
	byID     map[string]int

	// The composer.
	draft       string
	attachments []string
	focusDraft  bool

	// Chrome.
	list         ui.ListState
	sidebarOpen  bool
	settingsOpen bool
	modelOpen    bool
	permOpen     bool
	confirm      *confirmation
	expanded     map[string]bool // tool groups opened by the user
	copied       map[string]time.Time
	toastError   string
	settingsForm settingsForm

	md      markdown.Options
	p       palette
	scale   float32 // fonts: the text size setting times the interface zoom
	pxScale float32 // layout: the interface zoom alone

	// Called by commands that fail before the core is even asked; shown as
	// a toast instead of the status line, which is the core's.
	notice string
}

// confirmation is a destructive action waiting for the user to say yes.
type confirmation struct {
	title, body, action string
	destructive         bool
	run                 func()
}

func newApp() *app {
	return &app{
		byID:        map[string]int{},
		expanded:    map[string]bool{},
		copied:      map[string]time.Time{},
		sidebarOpen: true,
		scale:       1,
		pxScale:     1,
	}
}

// apply folds one poll into the copy this window draws from.
func (a *app) apply(u bridge.Update) {
	if u.Settings != nil {
		first := a.settings.Backend == ""
		a.settings = *u.Settings
		if first {
			a.sidebarOpen = a.settings.SidebarOpen
		}
		a.settingsForm.sync(a.settings)
	}
	if u.State != nil {
		a.state = *u.State
	}
	if u.Messages != nil {
		a.messages = *u.Messages
		a.byID = make(map[string]int, len(a.messages))
		for i, m := range a.messages {
			a.byID[m.ID] = i
		}
	}
	for _, d := range u.Deltas {
		if i, ok := a.byID[d.ID]; ok {
			a.messages[i].Content = d.Content
		}
	}
	for _, e := range u.Composer {
		switch e.Action {
		case "clear":
			a.draft = ""
			a.attachments = nil
		case "restore":
			// Whatever was typed since stays; the prompt that came back goes
			// in front of it, so nothing is lost either way.
			if strings.TrimSpace(a.draft) != "" && strings.TrimSpace(e.Text) != "" {
				a.draft = e.Text + "\n\n" + a.draft
			} else if strings.TrimSpace(e.Text) != "" {
				a.draft = e.Text
			}
			a.attachments = mergePaths(a.attachments, e.Attachments)
			a.focusDraft = true
		}
	}
	if u.Error != "" {
		a.notice = u.Error
	}
}

func mergePaths(have, add []string) []string {
	seen := map[string]bool{}
	for _, p := range have {
		seen[p] = true
	}
	for _, p := range add {
		if !seen[p] {
			have = append(have, p)
			seen[p] = true
		}
	}
	return have
}

// pump waits on the core and redraws as soon as it has something: a delta
// is on screen within a frame of arriving, and an idle app sleeps.
func (a *app) pump() {
	for {
		bridge.Wait(1000)
		u, err := bridge.Poll()
		if err != nil {
			log.Printf("poll: %v", err)
			continue
		}
		if u.State == nil && u.Messages == nil && len(u.Deltas) == 0 && u.Settings == nil && len(u.Composer) == 0 && u.Error == "" {
			continue
		}
		a.win.Update(func() { a.apply(u) })
	}
}

// do sends a command, and shows why if it was refused.
func (a *app) do(cmd string, args map[string]any) {
	if err := bridge.Command(cmd, args, nil); err != nil {
		a.notice = err.Error()
	}
}

// setting writes preferences, keeping the reply's view of them.
func (a *app) setting(args map[string]any) error {
	var reply struct {
		Settings *bridge.Settings `json:"settings"`
	}
	if err := bridge.Command("settings", args, &reply); err != nil {
		return err
	}
	if reply.Settings != nil {
		a.settings = *reply.Settings
	}
	return nil
}

func (a *app) connected() bool { return a.state.Connection.State == "connected" }

func (a *app) backendName() string {
	if a.state.Backend.Name != "" {
		return a.state.Backend.Name
	}
	return "Agent"
}
