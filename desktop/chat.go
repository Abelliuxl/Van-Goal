package main

import (
	"fmt"
	"math"
	"path/filepath"
	"strings"
	"time"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"
	"github.com/egoist/mygo/ui/markdown"

	"github.com/Abelliuxl/Van-Goal/desktop/bridge"
)

// The reading column: wide enough for code, narrow enough to read.
const columnWidth = 760

func (a *app) chat(c *ui.Context) {
	ui.Column(c).Grow(1).MinHeight(0).Children(func() {
		if len(a.messages) == 0 {
			a.welcome(c)
		} else {
			a.transcript(c)
		}
		a.composerArea(c)
	})
}

// ---------------------------------------------------------------- welcome

func (a *app) welcome(c *ui.Context) {
	p := a.p
	ui.Column(c).Grow(1).AlignItems(ui.Center).Justify(ui.Center).Padding(0, a.px(32), a.px(40), a.px(32)).Gap(a.px(10)).Children(func() {
		mark := ui.Box(c).Size(a.px(44), a.px(44)).Radius(a.px(14)).Center().Background(p.accentBg)
		mark.Children(func() { ui.Icon(c, iconSpark).FontSize(a.size(22)).TextColor(p.accent) })
		mark.Transition(ui.ElementTransition{Enter: &ui.Motion{Opacity: 0, Scale: 0.9}})

		headline := "What are we working on?"
		sub := fmt.Sprintf("Talking to %s. Ask for a change, a review, or an explanation.", a.backendName())
		switch {
		case !a.state.Backend.Enabled:
			headline = a.backendName() + " is switched off"
			sub = "Turn it on in Settings to start chatting."
		case a.state.Connection.State == "failed" && !a.state.Reconnecting:
			headline = "Can’t reach " + a.backendName()
			if a.state.Error != nil {
				sub = *a.state.Error
			}
		case a.state.Connection.State == "connecting" || a.state.Reconnecting:
			sub = "Connecting to " + a.backendName() + "…"
		}
		ui.Text(c, headline).FontSize(a.size(24)).FontWeight(600).TextColor(p.text).Margin(a.px(8), 0, 0, 0).TextAlign(ui.Center)
		ui.Text(c, sub).FontSize(a.size(14)).TextColor(p.muted).TextAlign(ui.Center).MaxWidth(a.px(460))
		if !a.state.Backend.Enabled || (a.state.Connection.State == "failed" && !a.state.Reconnecting) {
			ui.Row(c).Gap(a.px(8)).Margin(a.px(14), 0, 0, 0).Children(func() {
				if !a.state.Backend.Enabled {
					if ui.PrimaryButton(c, "Turn on "+a.backendName()).Radius(a.px(999)).Padding(a.px(8), a.px(16)).Clicked() {
						a.do("connect", nil)
					}
				} else if ui.PrimaryButton(c, "Try again").Radius(a.px(999)).Padding(a.px(8), a.px(16)).Clicked() {
					a.do("connect", nil)
				}
				if ui.Button(c, "Settings").Radius(a.px(999)).Padding(a.px(8), a.px(16)).Clicked() {
					a.openSettings()
				}
			})
		}
	})
}

// ------------------------------------------------------------- transcript

func (a *app) transcript(c *ui.Context) {
	p := a.p
	a.list.Key = func(i int) any { return a.messages[i].ID }
	a.list.FollowEnd = true
	a.list.FollowAnimation = &ui.Smooth

	ui.Box(c).Grow(1).MinHeight(0).Children(func() {
		ui.List(c, &a.list, len(a.messages), func(i int) {
			ui.Row(c).Justify(ui.Center).Padding(0, a.px(28)).Children(func() {
				ui.Column(c).Grow(1).MaxWidth(a.px(columnWidth)).MinWidth(0).Children(func() {
					a.message(c, i)
				})
			})
		}).Fill().Padding(a.px(12), 0, a.px(28), 0).Gap(a.px(22)).Justify(ui.End)

		// Back to the latest, when the user scrolled up from it (and only
		// once the list has been laid out at all).
		if _, last := a.list.Visible(); last >= 0 && !a.list.AtEnd() {
			jump := ui.ButtonBase(c).Size(a.px(34), a.px(34)).Radius(a.px(17)).Center().Label("Jump to latest").
				Background(p.surface).Border(1, p.line).Shadow(0, 4, 14, 0, p.shadow).
				Attach(ui.AnchorBottom, ui.AnchorBottom).Bottom(a.px(14))
			jump.Transition(ui.ElementTransition{Enter: &ui.Motion{Opacity: 0, Y: 8}, Exit: &ui.Motion{Opacity: 0, Y: 8}})
			if jump.Hovered() {
				jump.Background(p.raised)
			}
			jump.Children(func() { ui.Icon(c, iconArrowDown).FontSize(a.size(16)).TextColor(p.muted) })
			if jump.Clicked() {
				a.list.ScrollToEnd()
			}
		}
	})
}

func (a *app) message(c *ui.Context, i int) {
	m := a.messages[i]
	switch m.Role {
	case "user":
		a.userMessage(c, m)
	case "assistant":
		a.assistantMessage(c, i, m)
	default:
		ui.Text(c, m.Content).Selectable().FontSize(a.size(12.5)).TextColor(a.p.muted).TextAlign(ui.Center).Padding(a.px(4), 0)
	}
}

func (a *app) userMessage(c *ui.Context, m bridge.Message) {
	p := a.p
	ui.Column(c).AlignItems(ui.End).Gap(a.px(6)).Children(func() {
		if len(m.Attachments) > 0 {
			ui.Row(c).Gap(a.px(6)).Wrap().Justify(ui.End).Children(func() {
				for _, at := range m.Attachments {
					a.fileChip(c, at.Name, at.Path, nil)
				}
			})
		}
		if strings.TrimSpace(m.Content) != "" {
			ui.Text(c, m.Content).Selectable().FontSize(a.size(15)).LineHeight(1.55).TextColor(p.text).
				Padding(a.px(10), a.px(15)).Radius(a.px(18), a.px(18), a.px(6), a.px(18)).Background(p.userBg).MaxWidth(a.px(columnWidth * 0.78)).
				SelectionColor(p.accent.Alpha(0.28))
		}
	})
}

func (a *app) assistantMessage(c *ui.Context, i int, m bridge.Message) {
	last := i == len(a.messages)-1
	ui.Column(c).Gap(a.px(10)).Children(func() {
		if len(m.Tools) > 0 && a.settings.ShowToolCalls {
			a.toolGroup(c, m)
		}
		content := strings.TrimSpace(m.Content)
		switch {
		case content == "" && m.Streaming:
			a.thinking(c, m)
		case content != "":
			if strings.HasPrefix(content, "Error: ") && !m.Streaming {
				a.errorCard(c, strings.TrimPrefix(content, "Error: "))
			} else if m.Streaming {
				markdown.Stream(c.Key("md:"+m.ID), m.Content, &a.md)
			} else {
				markdown.View(c.Key("md:"+m.ID), m.Content, &a.md)
			}
		}
		if !m.Streaming && content != "" {
			a.messageActions(c, m, last)
		}
	})
}

// thinking is the placeholder while a reply has not produced text yet: three
// dots breathing, and how long it has been.
func (a *app) thinking(c *ui.Context, m bridge.Message) {
	p := a.p
	ui.Row(c).Gap(a.px(10)).AlignItems(ui.Center).Padding(a.px(4), 0).Children(func() {
		ui.Row(c).Gap(a.px(5)).AlignItems(ui.Center).Children(func() {
			for k := 0; k < 3; k++ {
				dot := ui.Box(c.Key(k)).Size(a.px(6), a.px(6)).Radius(a.px(3)).Background(p.muted)
				phase := dot.Loop("breathe", 1200*time.Millisecond, ui.Linear)
				x := math.Mod(float64(phase)+1-float64(k)*0.16, 1)
				dot.Opacity(float32(0.25 + 0.75*math.Pow(math.Sin(x*math.Pi), 2)))
			}
		})
		label := "Thinking"
		if a.state.Stopping {
			label = "Stopping"
		}
		if secs := time.Since(unixTime(m.Timestamp)).Seconds(); m.Timestamp > 0 && secs >= 3 && secs < 36000 {
			label += " · " + duration(secs)
			c.After(time.Second)
		}
		ui.Text(c, label).FontSize(a.size(13)).TextColor(p.muted)
	})
}

func (a *app) errorCard(c *ui.Context, text string) {
	p := a.p
	ui.Row(c).Gap(a.px(10)).AlignItems(ui.Start).Padding(a.px(10), a.px(14)).Radius(a.px(12)).Background(p.danger.Alpha(0.08)).Border(1, p.danger.Alpha(0.22)).Children(func() {
		ui.Icon(c, iconWarn).FontSize(a.size(16)).TextColor(p.danger).Margin(a.px(2), 0, 0, 0)
		ui.Text(c, text).Selectable().FontSize(a.size(13.5)).LineHeight(1.5).TextColor(p.text).Grow(1).MinWidth(0)
	})
}

// messageActions sits under a finished reply: copy, and how long it took.
// It shows on the last reply always and on the others while hovered, so the
// transcript reads as text rather than as a row of buttons.
func (a *app) messageActions(c *ui.Context, m bridge.Message, last bool) {
	p := a.p
	row := ui.Row(c).Gap(a.px(4)).AlignItems(ui.Center).Height(a.px(26)).Margin(a.px(-4), 0, 0, a.px(-6))
	row.Children(func() {
		copied := time.Since(a.copied[m.ID]) < 1500*time.Millisecond
		b := ui.ButtonBase(c).Size(a.px(26), a.px(26)).Radius(a.px(6)).Center().Label("Copy").Tooltip("Copy")
		if b.Hovered() {
			b.Background(p.raised)
		}
		b.Children(func() {
			if copied {
				ui.Icon(c, iconCheck).FontSize(a.size(15)).TextColor(p.ok)
			} else {
				ui.Icon(c, iconCopy).FontSize(a.size(15)).TextColor(p.faint)
			}
		})
		if b.Clicked() {
			mygo.Clipboard.WriteText(m.Content)
			a.copied[m.ID] = time.Now()
			c.After(1600 * time.Millisecond)
		}
		if m.CompletedAt != nil && *m.CompletedAt > m.Timestamp {
			secs := *m.CompletedAt - m.Timestamp
			if secs >= 1 && secs < 36000 {
				ui.Text(c, duration(secs)).FontSize(a.size(11.5)).TextColor(p.faint).Padding(0, a.px(4))
			}
		}
	})
	if !last && !row.Hovered() {
		row.Opacity(0.0)
	}
	row.Transition(ui.ElementTransition{Colors: true, Duration: 120 * time.Millisecond})
}

// toolGroup folds a turn's tool calls into one line that opens to the list.
func (a *app) toolGroup(c *ui.Context, m bridge.Message) {
	p := a.p
	open := a.expanded[m.ID]
	running := m.Streaming && strings.TrimSpace(m.Content) == ""
	ui.Column(c.Key("tools:" + m.ID)).Gap(a.px(6)).Children(func() {
		head := ui.ButtonBase(c).AlignSelf(ui.Start).Padding(a.px(6), a.px(12), a.px(6), a.px(10)).Radius(a.px(999)).Gap(a.px(8)).AlignItems(ui.Center).Background(p.codeBg).Border(1, p.line).Label("Tool calls")
		if head.Hovered() {
			head.Background(p.raised)
		}
		head.Children(func() {
			if running {
				ui.Spinner(c).Size(a.px(12), a.px(12))
			} else {
				ui.Icon(c, iconWrench).FontSize(a.size(13)).TextColor(p.muted)
			}
			last := m.Tools[len(m.Tools)-1]
			label := fmt.Sprintf("%d tool call", len(m.Tools))
			if len(m.Tools) != 1 {
				label += "s"
			}
			ui.Text(c, label).FontSize(a.size(12.5)).FontWeight(550).TextColor(p.text)
			if !open {
				ui.Text(c, last.Name).SingleLine().FontSize(a.size(12.5)).TextColor(p.faint).MinWidth(0).Shrink(1).Font(monoFont())
			}
			chev := ui.Icon(c, iconChevron).FontSize(a.size(13)).TextColor(p.faint)
			chev.Rotate(chev.Animate("open", map[bool]float32{true: 90, false: 0}[open], 160*time.Millisecond))
		})
		if head.Clicked() {
			a.expanded[m.ID] = !open
		}
		if open {
			ui.Column(c).Gap(a.px(6)).Padding(0, 0, 0, a.px(12)).BorderWidth(0, 0, 0, 2).BorderColor(p.line.Alpha(1.6)).
				Transition(ui.ElementTransition{Enter: &ui.Motion{Opacity: 0, Collapse: true}, Exit: &ui.Motion{Opacity: 0, Collapse: true}}).
				Children(func() {
					shown := m.Tools
					if len(shown) > 40 {
						shown = shown[len(shown)-40:]
						ui.Text(c, fmt.Sprintf("%d earlier calls not shown", len(m.Tools)-40)).FontSize(a.size(11.5)).TextColor(p.faint)
					}
					for _, t := range shown {
						a.toolCall(c, t)
					}
				})
		}
	})
}

func (a *app) toolCall(c *ui.Context, t bridge.Tool) {
	p := a.p
	key := "tool:" + t.ID + t.Status
	open := a.expanded[key]
	ui.Column(c.Key(key)).Gap(a.px(4)).Children(func() {
		head := ui.ButtonBase(c).Gap(a.px(8)).AlignItems(ui.Center).Padding(a.px(3), a.px(4)).Radius(a.px(6)).Label(t.Name)
		if head.Hovered() {
			head.Background(p.raised)
		}
		head.Children(func() {
			ui.Box(c).Size(a.px(6), a.px(6)).Radius(a.px(3)).Background(toolStatusColor(p, t.Status))
			ui.Text(c, t.Name).SingleLine().FontSize(a.size(12.5)).FontWeight(500).TextColor(p.text).Font(monoFont())
			if t.Status != "" {
				ui.Text(c, t.Status).SingleLine().FontSize(a.size(11.5)).TextColor(p.faint)
			}
		})
		if head.Clicked() && t.Detail != "" {
			a.expanded[key] = !open
		}
		if open && t.Detail != "" {
			detail := t.Detail
			if t.Truncated {
				detail += "\n…"
			}
			ui.Scroll(c).MaxHeight(a.px(260)).Radius(a.px(8)).Background(p.codeBg).Border(1, p.line).Children(func() {
				ui.Text(c, detail).Selectable().NoWrap().Font(monoFont()).FontSize(a.size(12)).LineHeight(1.45).TextColor(p.muted).Padding(a.px(10), a.px(12))
			})
		}
	})
}

func toolStatusColor(p palette, status string) ui.Color {
	s := strings.ToLower(status)
	switch {
	case strings.Contains(s, "error") || strings.Contains(s, "fail"):
		return p.danger
	case strings.Contains(s, "run") || strings.Contains(s, "pend") || strings.Contains(s, "start") || strings.Contains(s, "progress"):
		return p.warn
	}
	return p.ok
}

// ---------------------------------------------------------------- composer

func (a *app) composerArea(c *ui.Context) {
	ui.Row(c).Justify(ui.Center).Padding(0, a.px(28), a.px(18), a.px(28)).Children(func() {
		ui.Column(c).Grow(1).MaxWidth(a.px(columnWidth)).Gap(a.px(8)).Children(func() {
			if a.state.Clarify != nil {
				a.clarifyCard(c)
			}
			if len(a.state.Queue) > 0 {
				a.queue(c)
			}
			a.composer(c)
			a.footnote(c)
		})
	})
}

func (a *app) composer(c *ui.Context) {
	p := a.p
	s := a.state
	box := ui.Column(c).Radius(a.px(18)).Background(p.surface).Border(1, p.line.Alpha(1.4)).
		Shadow(0, 6, 24, 0, p.shadow).Shadow(0, 1, 2, 0, p.shadow.Alpha(0.6)).Padding(a.px(6), a.px(6), a.px(6), a.px(6))
	files := box.DroppedFiles()
	if len(files) > 0 {
		a.attachments = mergePaths(a.attachments, files)
	}
	if box.FileDragOver() {
		box.Border(1.5, p.accent)
	}
	box.Children(func() {
		if len(a.attachments) > 0 {
			ui.Row(c).Gap(a.px(6)).Wrap().Padding(a.px(6), a.px(8), a.px(2), a.px(8)).Children(func() {
				for _, path := range a.attachments {
					path := path
					a.fileChip(c, filepath.Base(path), path, func() {
						out := a.attachments[:0:0]
						for _, other := range a.attachments {
							if other != path {
								out = append(out, other)
							}
						}
						a.attachments = out
					})
				}
			})
		}

		placeholder := "Message " + a.backendName()
		if s.Clarify != nil {
			placeholder = "Type your answer…"
		} else if s.Sending {
			placeholder = "Queue a follow-up…"
		}
		in := ui.TextArea(c.Key("composer"), &a.draft).Lines(1, 10).Placeholder(placeholder).Label("Message").
			Background(ui.Transparent).Border(0, ui.Transparent).Padding(a.px(10), a.px(12), a.px(6), a.px(12)).
			FontSize(a.size(15)).LineHeight(1.5).FocusRing(false)
		in.OnTextKey(func(k ui.TextKey) bool {
			if k.Key == ui.KeyEnter && k.Mods == 0 {
				a.submit(k.Text())
				return true
			}
			return false
		})
		in.OnPaste(func(text string) bool {
			// A pasted file path list from the file manager becomes attachments.
			return false
		})
		if a.focusDraft {
			in.Focus()
			a.focusDraft = false
		}

		ui.Row(c).Gap(a.px(4)).AlignItems(ui.Center).Padding(a.px(2), a.px(4), a.px(2), a.px(6)).Children(func() {
			a.iconButton(c, iconPaperclip, "Attach files", a.pickFiles)
			if s.Backend.Caps.Permissions {
				a.permissionPicker(c)
			}
			ui.Spacer(c)
			if s.Backend.Caps.Models && len(s.Model.Groups) > 0 {
				a.modelPicker(c)
			}
			a.sendButton(c)
		})
	})
}

func (a *app) sendButton(c *ui.Context) {
	p := a.p
	s := a.state
	ready := strings.TrimSpace(a.draft) != "" || len(a.attachments) > 0
	stop := s.Sending && !ready && s.Clarify == nil
	label := "Send"
	if stop {
		label = "Stop"
	} else if s.Sending && s.Clarify == nil {
		label = "Queue"
	}
	hint := map[string]string{
		"Send": "Send", "Stop": "Stop", "Queue": "Queue after this reply",
	}[label]
	if stop {
		hint += "  Esc"
	} else if isMac() {
		hint += "  ↩"
	} else {
		hint += "  Enter"
	}
	b := ui.ButtonBase(c.Key("send")).Size(a.px(34), a.px(34)).Radius(a.px(17)).Center().Label(label).Tooltip(hint)
	switch {
	case stop:
		b.Background(p.text)
	case ready:
		b.Background(p.accent)
		if b.Hovered() {
			b.Background(p.accent.Mix(p.text, 0.12))
		}
	default:
		b.Background(p.line.Alpha(2.2))
	}
	b.Transition(ui.ElementTransition{Colors: true, Duration: 140 * time.Millisecond})
	b.Children(func() {
		if stop {
			ui.Icon(c, iconStop).FontSize(a.size(14)).TextColor(p.bg)
		} else {
			color := ui.Hex("#ffffff")
			if !ready {
				color = p.faint
			}
			ui.Icon(c, iconArrowUp).FontSize(a.size(17)).TextColor(color)
		}
	})
	if b.Clicked() {
		if stop {
			a.do("stop", nil)
		} else {
			a.submit(a.draft)
		}
	}
}

func (a *app) submit(text string) {
	if strings.TrimSpace(text) == "" && len(a.attachments) == 0 {
		return
	}
	// The core says when the draft is taken (or gives it back), so the
	// composer never loses text the core did not accept.
	a.do("send", map[string]any{"text": text, "attachments": a.attachments})
}

func (a *app) pickFiles() {
	go func() {
		paths, err := mygo.Dialog.Open(mygo.OpenDialogOptions{
			Parent:   a.win,
			Title:    "Attach files",
			Multiple: true,
		})
		if err != nil || len(paths) == 0 {
			return
		}
		a.win.Update(func() {
			a.attachments = mergePaths(a.attachments, paths)
			a.focusDraft = true
		})
	}()
}

func (a *app) fileChip(c *ui.Context, name, path string, remove func()) {
	p := a.p
	chip := ui.Row(c.Key("chip:"+path)).Gap(a.px(6)).AlignItems(ui.Center).Padding(a.px(4), a.px(8), a.px(4), a.px(8)).Radius(a.px(8)).Background(p.codeBg).Border(1, p.line).Tooltip(path)
	chip.Transition(ui.ElementTransition{Enter: &ui.Motion{Opacity: 0, Scale: 0.92}})
	chip.Children(func() {
		ui.Icon(c, iconFile).FontSize(a.size(13)).TextColor(p.muted)
		ui.Text(c, name).SingleLine().MaxWidth(a.px(180)).FontSize(a.size(12.5)).TextColor(p.text)
		if remove != nil {
			x := ui.ButtonBase(c).Size(a.px(16), a.px(16)).Radius(a.px(8)).Center().Label("Remove " + name)
			if x.Hovered() {
				x.Background(p.raised)
			}
			x.Children(func() { ui.Icon(c, iconX).FontSize(a.size(11)).TextColor(p.muted) })
			if x.Clicked() {
				remove()
			}
		}
	})
}

// footnote is the one line under the composer: what went wrong, or how to
// send.
func (a *app) footnote(c *ui.Context) {
	p := a.p
	s := a.state
	ui.Row(c).Height(a.px(18)).Justify(ui.Center).AlignItems(ui.Center).Gap(a.px(6)).Children(func() {
		switch {
		case s.Error != nil && *s.Error != "":
			ui.Icon(c, iconWarn).FontSize(a.size(12)).TextColor(p.warn)
			ui.Text(c, *s.Error).SingleLine().Selectable().FontSize(a.size(12)).TextColor(p.warn).MinWidth(0).Shrink(1).Tooltip(*s.Error)
		case s.Sending:
			ui.Text(c, queueHint()).FontSize(a.size(11.5)).TextColor(p.faint)
		default:
			ui.Text(c, sendHint()).FontSize(a.size(11.5)).TextColor(p.faint)
		}
	})
}

func (a *app) clarifyCard(c *ui.Context) {
	p := a.p
	q := a.state.Clarify
	card := ui.Column(c.Key("clarify")).Gap(a.px(10)).Padding(a.px(14), a.px(16)).Radius(a.px(14)).Background(p.accentBg).Border(1, p.accent.Alpha(0.3))
	card.Transition(ui.ElementTransition{Enter: &ui.Motion{Opacity: 0, Y: 8}, Exit: &ui.Motion{Opacity: 0, Y: 8}})
	card.Children(func() {
		ui.Row(c).Gap(a.px(8)).AlignItems(ui.Center).Children(func() {
			ui.Icon(c, iconQuestion).FontSize(a.size(15)).TextColor(p.accent)
			ui.Text(c, a.backendName()+" is asking").FontSize(a.size(12)).FontWeight(600).TextColor(p.accent)
			ui.Spacer(c)
			if ui.Button(c, "Skip").Padding(a.px(3), a.px(10)).Clicked() {
				a.do("skip_question", nil)
			}
		})
		ui.Text(c, q.Question).Selectable().FontSize(a.size(14.5)).LineHeight(1.5).TextColor(p.text)
		if len(q.Choices) > 0 {
			ui.Row(c).Gap(a.px(8)).Wrap().Children(func() {
				for i, choice := range q.Choices {
					choice := choice
					b := ui.ButtonBase(c.Key(i)).Padding(a.px(6), a.px(12)).Radius(a.px(999)).Background(p.surface).Border(1, p.line.Alpha(1.6)).Gap(a.px(6)).AlignItems(ui.Center).Label(choice)
					if b.Hovered() {
						b.Border(1, p.accent)
					}
					b.Children(func() {
						ui.Text(c, fmt.Sprint(i+1)).FontSize(a.size(11)).TextColor(p.faint)
						ui.Text(c, choice).FontSize(a.size(13)).TextColor(p.text)
					})
					if b.Clicked() {
						a.do("answer", map[string]any{"text": choice})
					}
				}
			})
		}
	})
}

func (a *app) queue(c *ui.Context) {
	p := a.p
	ui.Column(c.Key("queue")).Gap(a.px(2)).Padding(a.px(6)).Radius(a.px(14)).Background(p.surface).Border(1, p.line).Children(func() {
		for _, item := range a.state.Queue {
			item := item
			row := ui.Row(c.Key(item.ID)).Gap(a.px(8)).AlignItems(ui.Center).Padding(a.px(6), a.px(8)).Radius(a.px(9))
			row.Transition(ui.ElementTransition{Enter: &ui.Motion{Opacity: 0, Collapse: true}, Exit: &ui.Motion{Opacity: 0, Collapse: true}})
			if row.Hovered() {
				row.Background(p.raised)
			}
			hovered := row.Hovered()
			row.Children(func() {
				ui.Icon(c, iconQueue).FontSize(a.size(14)).TextColor(p.faint)
				text := strings.TrimSpace(item.Text)
				if text == "" {
					text = strings.Join(item.Attachments, ", ")
				}
				ui.Text(c, text).SingleLine().FontSize(a.size(13)).TextColor(p.text).Grow(1).MinWidth(0)
				if hovered {
					if ui.Button(c, "Send now").Padding(a.px(2), a.px(8)).Clicked() {
						a.do("queue_send_now", map[string]any{"id": item.ID})
					}
					if ui.Button(c, "Edit").Padding(a.px(2), a.px(8)).Clicked() {
						a.do("queue_edit", map[string]any{"id": item.ID})
					}
				}
				x := ui.ButtonBase(c).Size(a.px(22), a.px(22)).Radius(a.px(6)).Center().Label("Remove from queue")
				if x.Hovered() {
					x.Background(p.selected)
				}
				x.Children(func() { ui.Icon(c, iconX).FontSize(a.size(12)).TextColor(p.muted) })
				if x.Clicked() {
					a.do("queue_cancel", map[string]any{"id": item.ID})
				}
			})
		}
	})
}

// ----------------------------------------------------------------- pickers

func (a *app) modelPicker(c *ui.Context) {
	p := a.p
	m := a.state.Model
	label := a.backendName()
	if m.Name != nil && *m.Name != "" {
		label = *m.Name
	}
	if m.Switching {
		label = "Switching…"
	}
	b := ui.ButtonBase(c.Key("model")).Padding(a.px(5), a.px(10)).Radius(a.px(8)).Gap(a.px(5)).AlignItems(ui.Center).Label("Model").MinWidth(0).Shrink(1)
	if b.Hovered() {
		b.Background(p.raised)
	}
	b.Children(func() {
		ui.Text(c, label).SingleLine().MaxWidth(a.px(220)).MinWidth(0).FontSize(a.size(12.5)).TextColor(p.muted)
		ui.Icon(c, iconChevDown).FontSize(a.size(12)).TextColor(p.faint)
	})
	b.Menu(func(menu *ui.Menu) {
		for _, g := range m.Groups {
			g := g
			menu.Submenu(g.Provider, func(sub *ui.Menu) {
				for _, model := range g.Models {
					current := g.Provider == m.Provider && m.Name != nil && *m.Name == model
					if sub.Item(model).Checked(current).Chosen() {
						a.do("select_model", map[string]any{"provider": g.Provider, "model": model})
					}
				}
			})
		}
	})
}

func (a *app) permissionPicker(c *ui.Context) {
	p := a.p
	perm := a.state.Permission
	label := perm.Label
	if perm.Changing {
		label += "…"
	}
	b := ui.ButtonBase(c.Key("perm")).Padding(a.px(5), a.px(10)).Radius(a.px(8)).Gap(a.px(6)).AlignItems(ui.Center).Label("Permissions")
	if b.Hovered() {
		b.Background(p.raised)
	}
	b.Children(func() {
		ui.Icon(c, iconShield).FontSize(a.size(13)).TextColor(p.muted)
		ui.Text(c, label).SingleLine().FontSize(a.size(12.5)).TextColor(p.muted)
	})
	b.Menu(func(menu *ui.Menu) {
		for _, opt := range []struct{ id, label string }{{"full", "Full access"}, {"ask", "Ask first"}, {"restricted", "Restricted"}} {
			if menu.Item(opt.label).Checked(perm.Mode == opt.id).Chosen() {
				a.do("set_permission", map[string]any{"mode": opt.id})
			}
		}
	})
}

// ------------------------------------------------------------------ helpers

func unixTime(seconds float64) time.Time {
	return time.Unix(0, int64(seconds*float64(time.Second)))
}

func duration(seconds float64) string {
	s := int(math.Round(seconds))
	switch {
	case s < 60:
		return fmt.Sprintf("%ds", s)
	case s < 3600:
		return fmt.Sprintf("%dm %ds", s/60, s%60)
	}
	return fmt.Sprintf("%dh %dm", s/3600, (s%3600)/60)
}

func monoFont() string {
	if isMac() {
		return "SF Mono"
	}
	return "Consolas"
}
