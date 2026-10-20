package main

import (
	"fmt"
	"math"
	"runtime"
	"strings"
	"time"

	"github.com/egoist/mygo/ui"

	"github.com/Abelliuxl/Van-Goal/desktop/bridge"
)

const sidebarWidth = 264

func (a *app) view(c *ui.Context) {
	a.applyTheme(c)
	p := a.p
	a.shortcuts(c)

	ui.Row(c).Fill().AlignItems(ui.Stretch).Background(p.bg).Children(func() {
		if a.sidebarOpen {
			a.sidebar(c)
		}
		ui.Column(c).Grow(1).MinWidth(0).Background(p.bg).Children(func() {
			a.header(c)
			a.chat(c)
		})
	})
	a.settingsDialog(c)
	a.confirmDialog(c)
	a.toasts(c)
}

func (a *app) shortcuts(c *ui.Context) {
	switch {
	case c.Shortcut(ui.Cmd, ui.KeyN):
		a.newChat()
	case c.Shortcut(ui.Cmd, ui.KeyR):
		a.do("refresh", nil)
	case c.Shortcut(ui.Cmd, ui.KeyB):
		a.toggleSidebar()
	case c.Shortcut(ui.Cmd, ui.KeyEqual):
		// The menu binds ⌘+ (which needs Shift); Cmd+= is what other macOS
		// apps accept without it.
		a.zoomIn()
	case c.Shortcut(ui.Cmd, ui.KeyComma):
		a.openSettings()
	case c.Shortcut(0, ui.KeyEscape) && a.state.Sending:
		a.do("stop", nil)
	}
}

func (a *app) newChat() {
	a.do("new_chat", nil)
	a.focusDraft = true
}

func (a *app) toggleSidebar() {
	a.sidebarOpen = !a.sidebarOpen
	_ = a.setting(map[string]any{"sidebar_open": a.sidebarOpen})
}

// titleRoom is the room the window controls take, and the height the bars
// that sit under them share.
func (a *app) titleRoom(c *ui.Context) (height, left float32) {
	bar := c.TitleBar()
	return max(bar.Height, a.px(46)), bar.Left
}

// ---------------------------------------------------------------- sidebar

func (a *app) sidebar(c *ui.Context) {
	p := a.p
	height, left := a.titleRoom(c)
	ui.Column(c).Width(a.px(sidebarWidth)).Background(p.sidebar).BorderWidth(0, 1, 0, 0).BorderColor(p.line).Children(func() {
		// The bar under the traffic lights: drag the window, toggle the list.
		ui.Row(c).Height(height).PaddingStart(max(left, a.px(12))).PaddingEnd(a.px(10)).AlignItems(ui.Center).DragWindow().Children(func() {
			ui.Spacer(c)
			a.iconButton(c, iconSidebar, "Hide sidebar  ⌘B", a.toggleSidebar)
		})

		ui.Column(c).Padding(a.px(4), a.px(12), a.px(10), a.px(12)).Gap(a.px(2)).Children(func() {
			a.navRow(c, iconCompose, "New chat", "⌘N", a.newChat)
		})

		ui.Row(c).Padding(a.px(10), a.px(20), a.px(6), a.px(20)).AlignItems(ui.Center).Children(func() {
			ui.Text(c, "Chats").FontSize(a.size(11.5)).FontWeight(600).TextColor(p.faint).LetterSpacing(0.4)
			ui.Spacer(c)
			if a.state.Refreshing {
				ui.Spinner(c).Size(a.px(12), a.px(12))
			}
		})
		a.sessionList(c)
		a.connectionFooter(c)
	})
}

func (a *app) navRow(c *ui.Context, svg *ui.SVG, label, hint string, onClick func()) {
	p := a.p
	row := ui.ButtonBase(c.Key(label)).Padding(a.px(7), a.px(10)).Radius(a.px(8)).Gap(a.px(10)).AlignItems(ui.Center).Label(label)
	if row.Hovered() {
		row.Background(p.raised)
	}
	row.Transition(ui.ElementTransition{Colors: true, Duration: 90 * time.Millisecond})
	row.Children(func() {
		ui.Icon(c, svg).FontSize(a.size(16)).TextColor(p.text)
		ui.Text(c, label).FontSize(a.size(13.5)).FontWeight(500).TextColor(p.text).Grow(1)
		if hint != "" && row.Hovered() {
			ui.Text(c, hint).FontSize(a.size(11.5)).TextColor(p.faint)
		}
	})
	if row.Clicked() {
		onClick()
	}
}

func (a *app) sessionList(c *ui.Context) {
	p := a.p
	sessions := a.state.Sessions
	ui.Scroll(c).Grow(1).Padding(0, a.px(8), a.px(8), a.px(8)).Children(func() {
		if len(sessions) == 0 {
			ui.Column(c).Padding(a.px(24), a.px(14)).Gap(a.px(6)).Children(func() {
				ui.Text(c, "No chats yet").FontSize(a.size(13)).FontWeight(500).TextColor(p.muted)
				ui.Text(c, "Send a message to start one.").FontSize(a.size(12)).TextColor(p.faint)
			})
			return
		}
		group := ""
		for _, s := range sessions {
			if g := dayGroup(s.LastActive); g != group {
				group = g
				ui.Text(c.Key("group:"+g), g).FontSize(a.size(11)).FontWeight(500).TextColor(p.faint).Padding(a.px(12), a.px(12), a.px(4), a.px(12))
			}
			a.sessionRow(c, s)
		}
	})
}

func (a *app) sessionRow(c *ui.Context, s bridge.Session) {
	p := a.p
	selected := a.state.Selected != nil && a.state.Selected.ID == s.ID
	row := ui.ButtonBase(c.Key(s.ID)).Padding(a.px(8), a.px(10)).Radius(a.px(8)).Gap(a.px(8)).AlignItems(ui.Center).Label(s.Title)
	switch {
	case selected:
		row.Background(p.selected)
	case row.Hovered():
		row.Background(p.raised)
	}
	row.Transition(ui.ElementTransition{Colors: true, Duration: 90 * time.Millisecond})
	hovered := row.Hovered()
	row.Children(func() {
		ui.Column(c).Grow(1).MinWidth(0).Gap(a.px(2)).Children(func() {
			ui.Text(c, s.Title).SingleLine().FontSize(a.size(13.5)).FontWeight(map[bool]int{true: 600, false: 450}[selected]).TextColor(p.text)
			if s.Subtitle != "" {
				ui.Text(c, s.Subtitle).SingleLine().FontSize(a.size(11.5)).TextColor(p.faint)
			}
		})
		if s.Active {
			ui.Box(c).Size(a.px(7), a.px(7)).Radius(a.px(4)).Background(p.ok).Tooltip("Running")
		}
		if hovered || selected {
			more := ui.ButtonBase(c).Size(a.px(22), a.px(22)).Radius(a.px(6)).Center().Label("More").Cursor(ui.CursorPointer)
			if more.Hovered() {
				more.Background(p.selected.Mix(p.text, 0.08))
			}
			more.Children(func() { ui.Icon(c, iconMore).FontSize(a.size(15)).TextColor(p.muted) })
			more.Menu(func(m *ui.Menu) {
				if m.Item("Archive").Chosen() {
					a.ask(fmt.Sprintf("Archive “%s”?", s.Title), "It leaves the list; its transcript is kept.", "Archive", false, func() {
						a.do("archive", map[string]any{"id": s.ID})
					})
				}
				if m.Item("Delete").Chosen() {
					a.ask(fmt.Sprintf("Delete “%s”?", s.Title), "It leaves the list, and this app forgets its transcript. The backend keeps the session.", "Delete", true, func() {
						a.do("delete", map[string]any{"id": s.ID})
					})
				}
			})
		}
	})
	if row.Clicked() && !selected {
		a.do("open_session", map[string]any{"id": s.ID})
		a.focusDraft = true
	}
}

// connectionFooter is the backend at the bottom of the list: which one, how
// it is doing, and the way to settings.
func (a *app) connectionFooter(c *ui.Context) {
	p := a.p
	ui.Column(c).Padding(a.px(8), a.px(12), a.px(12), a.px(12)).BorderWidth(1, 0, 0, 0).BorderColor(p.line).Children(func() {
		row := ui.ButtonBase(c).Padding(a.px(8), a.px(10)).Radius(a.px(8)).Gap(a.px(10)).AlignItems(ui.Center).Label("Settings")
		if row.Hovered() {
			row.Background(p.raised)
		}
		row.Children(func() {
			a.statusDot(c, 8)
			ui.Column(c).Grow(1).MinWidth(0).Gap(a.px(1)).Children(func() {
				ui.Text(c, a.backendName()).SingleLine().FontSize(a.size(13)).FontWeight(550).TextColor(p.text)
				ui.Text(c, a.connectionWords()).SingleLine().FontSize(a.size(11.5)).TextColor(p.faint)
			})
			ui.Icon(c, iconSettings).FontSize(a.size(16)).TextColor(p.muted)
		})
		if row.Clicked() {
			a.openSettings()
		}
	})
}

func (a *app) connectionWords() string {
	s := a.state
	switch {
	case !s.Backend.Enabled:
		return "Switched off"
	case s.Connection.State == "connected":
		return "Connected"
	case s.Connection.State == "connecting":
		return "Connecting…"
	case s.Reconnecting:
		return "Reconnecting…"
	case s.Connection.State == "degraded":
		return "Limited"
	case s.Connection.State == "failed":
		return "Can’t connect"
	}
	return "Offline"
}

func (a *app) statusColor() ui.Color {
	p := a.p
	if !a.state.Backend.Enabled {
		return p.faint
	}
	switch a.state.Connection.State {
	case "connected":
		return p.ok
	case "connecting":
		return p.warn
	case "degraded":
		return p.warn
	case "failed":
		if a.state.Reconnecting {
			return p.warn
		}
		return p.danger
	}
	return p.faint
}

func (a *app) statusDot(c *ui.Context, size float32) {
	size = a.px(size)
	dot := ui.Box(c).Size(size, size).Radius(size / 2).Background(a.statusColor())
	if a.state.Connection.State == "connecting" || a.state.Reconnecting {
		pulse := dot.Loop("pulse", 1400*time.Millisecond, ui.Linear)
		dot.Opacity(float32(0.45 + 0.55*(0.5+0.5*math.Cos(float64(pulse)*2*math.Pi))))
	}
}

// ----------------------------------------------------------------- header

func (a *app) header(c *ui.Context) {
	p := a.p
	height, left := a.titleRoom(c)
	if a.sidebarOpen {
		left = 0
	}
	ui.Row(c).Height(height).PaddingStart(max(left+a.px(8), a.px(16))).PaddingEnd(a.px(14)).Gap(a.px(8)).AlignItems(ui.Center).DragWindow().Children(func() {
		if !a.sidebarOpen {
			a.iconButton(c, iconSidebar, "Show sidebar  ⌘B", a.toggleSidebar)
			a.iconButton(c, iconCompose, "New chat  ⌘N", a.newChat)
		}
		title := "New chat"
		if a.state.Selected != nil {
			title = a.state.Selected.Title
		}
		ui.Text(c, title).SingleLine().FontSize(a.size(14)).FontWeight(600).TextColor(p.text).MinWidth(0).Shrink(1).Padding(0, a.px(6))
		ui.Spacer(c)
		a.contextMeter(c)
		a.iconButton(c, iconRefresh, "Refresh  ⌘R", func() { a.do("refresh", nil) })
	})
}

func (a *app) contextMeter(c *ui.Context) {
	p := a.p
	ctx := a.state.Context
	if ctx.Max <= 0 || len(a.messages) == 0 {
		return
	}
	color := p.faint
	switch {
	case ctx.Ratio >= 0.9:
		color = p.danger
	case ctx.Ratio >= 0.72:
		color = p.warn
	}
	approx := "≈ "
	if ctx.Measured {
		approx = ""
	}
	tip := fmt.Sprintf("Context: %s%s of %s tokens", approx, compact(ctx.Used), compact(ctx.Max))
	ui.Row(c).Gap(a.px(6)).AlignItems(ui.Center).Padding(a.px(4), a.px(8)).Radius(a.px(6)).Tooltip(tip).Children(func() {
		ui.Box(c).Size(a.px(34), a.px(4)).Radius(a.px(2)).Background(p.line.Alpha(2)).Children(func() {
			ui.Box(c).Height(a.px(4)).Width(float32(math.Max(0.04, ctx.Ratio)) * a.px(34)).Radius(a.px(2)).Background(color)
		})
		ui.Text(c, fmt.Sprintf("%d%%", int(math.Round(ctx.Ratio*100)))).FontSize(a.size(11)).TextColor(p.faint).FontFeatures("tnum")
	})
}

// iconButton is a quiet, square button that shows only an icon.
func (a *app) iconButton(c *ui.Context, svg *ui.SVG, tip string, onClick func()) ui.Element {
	p := a.p
	b := ui.ButtonBase(c.Key(tip)).Size(a.px(30), a.px(30)).Radius(a.px(7)).Center().Label(tip).Tooltip(tip)
	if b.Hovered() {
		b.Background(p.raised)
	}
	b.Transition(ui.ElementTransition{Colors: true, Duration: 90 * time.Millisecond})
	b.Children(func() { ui.Icon(c, svg).FontSize(a.size(17)).TextColor(p.muted) })
	if b.Clicked() {
		onClick()
	}
	return b
}

// ------------------------------------------------------------------ helpers

func dayGroup(unix *float64) string {
	if unix == nil || *unix <= 0 {
		return "Earlier"
	}
	t := time.Unix(int64(*unix), 0)
	now := time.Now()
	today := time.Date(now.Year(), now.Month(), now.Day(), 0, 0, 0, 0, now.Location())
	switch {
	case !t.Before(today):
		return "Today"
	case !t.Before(today.AddDate(0, 0, -1)):
		return "Yesterday"
	case !t.Before(today.AddDate(0, 0, -7)):
		return "Previous 7 days"
	case !t.Before(today.AddDate(0, 0, -30)):
		return "Previous 30 days"
	}
	return "Earlier"
}

func compact(n int64) string {
	switch {
	case n >= 1_000_000:
		return trimZero(fmt.Sprintf("%.1f", float64(n)/1_000_000)) + "M"
	case n >= 1_000:
		return trimZero(fmt.Sprintf("%.1f", float64(n)/1_000)) + "k"
	}
	return fmt.Sprint(n)
}

func trimZero(s string) string { return strings.TrimSuffix(s, ".0") }

func isMac() bool { return runtime.GOOS == "darwin" }
