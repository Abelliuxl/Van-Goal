package main

import (
	"fmt"
	"strconv"
	"strings"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"

	"github.com/Abelliuxl/Van-Goal/desktop/bridge"
)

// settingsForm holds the text fields while they are being edited. A field is
// written to the core when it is committed (Enter, or leaving it), not on
// every keystroke: a half-typed host would otherwise be what a reconnect
// in the middle of typing uses.
type settingsForm struct {
	backend    string
	host       string
	port       string
	credential string
	workspace  string
	profile    string
	showSecret bool
	section    int
	focus      map[string]bool
	edited     map[string]bool
	err        string
	serverNote string
}

// sync refills the fields from the settings, when the backend they describe
// changed or a field is not being edited.
func (f *settingsForm) sync(s bridge.Settings) {
	if f.backend != s.Backend {
		f.backend = s.Backend
		f.host, f.port, f.credential = s.Host, strconv.Itoa(s.Port), s.Credential
		f.workspace, f.profile = s.Workspace, s.Profile
		f.err = ""
	}
}

func (a *app) openSettings() {
	a.settingsOpen = true
	a.settingsForm.backend = "" // refill from what is saved
	a.settingsForm.sync(a.settings)
	var reply struct {
		Message string `json:"message"`
	}
	if bridge.Command("server_message", nil, &reply) == nil {
		a.settingsForm.serverNote = reply.Message
	}
}

var settingsSections = []string{"Agent", "Appearance", "Advanced"}

func (a *app) settingsDialog(c *ui.Context) {
	if !a.settingsOpen {
		return
	}
	p := a.p
	ui.DialogBase(c, &a.settingsOpen, func(backdrop, panel ui.Element) {
		backdrop.Background(p.overlayBg)
		// The panel is sized in design pixels, so at a high zoom it can outgrow
		// a small window. Keep it inside what is on screen instead of letting
		// the bottom and sides clip.
		width, height := a.px(680), a.px(560)
		if cw, ch := c.Size(); width > cw-a.px(40) || height > ch-a.px(40) {
			width = min(width, max(cw-a.px(40), a.px(420)))
			height = min(height, max(ch-a.px(40), a.px(320)))
		}
		panel.Width(width).Height(height).Radius(a.px(16)).Background(p.bg).Border(1, p.line).
			Shadow(0, 24, 64, 0, p.shadow.Alpha(2)).Clip()
		panel.Transition(ui.ElementTransition{Enter: &ui.Motion{Opacity: 0, Scale: 0.97, Y: 6}})
		ui.Row(c).Fill().AlignItems(ui.Stretch).Children(func() {
			// Sections.
			ui.Column(c).Width(a.px(180)).Background(p.sidebar).BorderWidth(0, 1, 0, 0).BorderColor(p.line).Padding(a.px(18), a.px(10)).Gap(a.px(2)).Children(func() {
				ui.Text(c, "Settings").FontSize(a.size(15)).FontWeight(650).TextColor(p.text).Padding(0, a.px(10), a.px(12), a.px(10))
				for i, name := range settingsSections {
					i := i
					row := ui.ButtonBase(c.Key(name)).Padding(a.px(7), a.px(10)).Radius(a.px(8)).Justify(ui.Start).Label(name)
					switch {
					case a.settingsForm.section == i:
						row.Background(p.selected)
					case row.Hovered():
						row.Background(p.raised)
					}
					row.Children(func() {
						ui.Text(c, name).FontSize(a.size(13.5)).FontWeight(map[bool]int{true: 600, false: 450}[a.settingsForm.section == i]).TextColor(p.text)
					})
					if row.Clicked() {
						a.settingsForm.section = i
					}
				}
			})
			// The section.
			ui.Column(c).Grow(1).MinWidth(0).Children(func() {
				ui.Row(c).Padding(a.px(14), a.px(14), 0, 0).Children(func() {
					ui.Spacer(c)
					a.iconButton(c, iconX, "Close", func() { a.settingsOpen = false })
				})
				ui.Scroll(c).Grow(1).Padding(0, a.px(28), a.px(28), a.px(28)).Gap(a.px(22)).Children(func() {
					switch a.settingsForm.section {
					case 0:
						a.agentSettings(c)
					case 1:
						a.appearanceSettings(c)
					case 2:
						a.advancedSettings(c)
					}
				})
			})
		})
	})
}

func (a *app) sectionTitle(c *ui.Context, title, hint string) {
	p := a.p
	ui.Column(c).Gap(a.px(4)).Children(func() {
		ui.Text(c, title).FontSize(a.size(17)).FontWeight(650).TextColor(p.text)
		if hint != "" {
			ui.Text(c, hint).FontSize(a.size(12.5)).LineHeight(1.45).TextColor(p.muted)
		}
	})
}

func (a *app) group(c *ui.Context, label string, fn func()) {
	p := a.p
	ui.Column(c.Key("group:" + label)).Gap(a.px(8)).Children(func() {
		if label != "" {
			ui.Text(c, label).FontSize(a.size(11.5)).FontWeight(600).TextColor(p.faint).LetterSpacing(0.3)
		}
		ui.Column(c).Radius(a.px(12)).Background(p.surface).Border(1, p.line).Clip().Children(fn)
	})
}

// field is one row of a group: a label on the left, the control on the right.
func (a *app) field(c *ui.Context, label, hint string, control func()) {
	p := a.p
	ui.Row(c.Key("field:"+label)).Gap(a.px(16)).Padding(a.px(12), a.px(14)).AlignItems(ui.Center).BorderWidth(0, 0, 1, 0).BorderColor(p.line).Children(func() {
		ui.Column(c).Width(a.px(170)).Gap(a.px(2)).Children(func() {
			ui.Text(c, label).FontSize(a.size(13)).FontWeight(500).TextColor(p.text)
			if hint != "" {
				ui.Text(c, hint).FontSize(a.size(11.5)).LineHeight(1.4).TextColor(p.faint)
			}
		})
		ui.Row(c).Grow(1).MinWidth(0).Gap(a.px(8)).AlignItems(ui.Center).Children(control)
	})
}

// input is a settings text field. It commits on Enter, and when the focus
// leaves it with an edit in it, once.
func (a *app) input(c *ui.Context, key string, value *string, placeholder string, password bool, commit func()) {
	p := a.p
	f := &a.settingsForm
	in := ui.TextInput(c.Key(key), value).Placeholder(placeholder).Grow(1).Label(key).
		Background(p.bg).Border(1, p.line.Alpha(1.6)).Radius(a.px(8)).Padding(a.px(6), a.px(10)).FontSize(a.size(13))
	if password {
		in.Password()
	}
	focused := in.Focused()
	if focused {
		in.Border(1, p.accent)
	}
	if f.focus == nil {
		f.focus, f.edited = map[string]bool{}, map[string]bool{}
	}
	if in.Changed() {
		f.edited[key] = true
	}
	left := f.focus[key] && !focused
	f.focus[key] = focused
	if in.Submitted() || (left && f.edited[key]) {
		f.edited[key] = false
		commit()
	}
}

func (a *app) commit(args map[string]any) {
	if err := a.setting(args); err != nil {
		a.settingsForm.err = err.Error()
		return
	}
	a.settingsForm.err = ""
}

func (a *app) agentSettings(c *ui.Context) {
	p := a.p
	s := a.settings
	f := &a.settingsForm
	a.sectionTitle(c, "Agent", "One backend runs at a time. Switching one on switches the others off, and the one you switch off stays off after a restart.")

	a.group(c, "BACKEND", func() {
		for _, b := range s.Backends {
			b := b
			active := b.ID == s.Backend
			row := ui.ButtonBase(c.Key("backend:"+b.ID)).Gap(a.px(12)).Padding(a.px(11), a.px(14)).AlignItems(ui.Center).BorderWidth(0, 0, 1, 0).BorderColor(p.line).Label(b.Name)
			if active {
				row.Background(p.accentBg)
			} else if row.Hovered() {
				row.Background(p.raised)
			}
			row.Children(func() {
				ring := ui.Box(c).Size(a.px(16), a.px(16)).Radius(a.px(8)).Border(1.5, p.line.Alpha(3)).Center()
				if active {
					ring.Border(1.5, p.accent).Children(func() { ui.Box(c).Size(a.px(8), a.px(8)).Radius(a.px(4)).Background(p.accent) })
				}
				ui.Column(c).Grow(1).MinWidth(0).Gap(a.px(2)).Children(func() {
					ui.Text(c, b.Name).FontSize(a.size(13.5)).FontWeight(550).TextColor(p.text)
					ui.Text(c, b.Description).SingleLine().FontSize(a.size(11.5)).TextColor(p.faint)
				})
				on := b.Enabled
				if ui.Switch(c, &on).Label(b.Name + " on").Changed() {
					if on {
						a.do("enable_backend", map[string]any{"backend": b.ID})
					} else {
						a.do("disable_backend", map[string]any{"backend": b.ID})
					}
				}
			})
			if row.Clicked() && !active {
				a.do("enable_backend", map[string]any{"backend": b.ID})
			}
		}
	})

	var current bridge.BackendOption
	for _, b := range s.Backends {
		if b.ID == s.Backend {
			current = b
		}
	}

	a.group(c, strings.ToUpper(current.Name)+" CONNECTION", func() {
		if current.Network {
			a.field(c, "Address", addressHint(s, current.ID), func() {
				a.input(c, "host", &f.host, "127.0.0.1 or https://…", false, func() {
					a.commit(map[string]any{"host": f.host})
				})
			})
			if !strings.Contains(f.host, "://") {
				a.field(c, "Port", "", func() {
					a.input(c, "port", &f.port, "", false, func() {
						a.commit(map[string]any{"port": f.port})
					})
					tls := s.UseTLS
					if ui.Switch(c, &tls).Label("TLS").Changed() {
						a.commit(map[string]any{"use_tls": tls})
					}
					ui.Text(c, "TLS").FontSize(a.size(12.5)).TextColor(p.muted)
				})
			}
			a.field(c, current.CredentialLabel, credentialHint(s, current.ID), func() {
				a.input(c, "credential", &f.credential, "Not set", !f.showSecret, func() {
					a.commit(map[string]any{"credential": f.credential})
				})
				eye := "Show"
				if f.showSecret {
					eye = "Hide"
				}
				if ui.Button(c, eye).Padding(a.px(4), a.px(10)).Clicked() {
					f.showSecret = !f.showSecret
				}
			})
		}
		if !current.Network || s.ManagedWorkspace {
			a.field(c, "Workspace", "The directory the agent works in.", func() {
				a.input(c, "workspace", &f.workspace, "~/Projects/…", false, func() {
					a.commit(map[string]any{"workspace": f.workspace})
					a.do("apply_workspace", nil)
				})
				if ui.Button(c, "Choose…").Padding(a.px(4), a.px(10)).Clicked() {
					go func() {
						paths, err := mygo.Dialog.Open(mygo.OpenDialogOptions{Parent: a.win, Title: "Choose the workspace", Directory: true})
						if err != nil || len(paths) == 0 {
							return
						}
						a.win.Update(func() {
							f.workspace = paths[0]
							a.commit(map[string]any{"workspace": f.workspace})
							a.do("apply_workspace", nil)
						})
					}()
				}
			})
		}
		if current.ID == "hermes" {
			a.field(c, "Profile", "Empty uses Hermes’ default.", func() {
				a.input(c, "profile", &f.profile, "default", false, func() {
					a.commit(map[string]any{"profile": f.profile})
				})
			})
		}
		ui.Row(c).Gap(a.px(8)).Padding(a.px(12), a.px(14)).AlignItems(ui.Center).Children(func() {
			a.statusDot(c, 8)
			ui.Text(c, a.state.Connection.Label).SingleLine().FontSize(a.size(12.5)).TextColor(p.muted).Grow(1).MinWidth(0).Tooltip(a.state.Connection.Label)
			if current.Enabled {
				if ui.Button(c, "Reconnect").Padding(a.px(4), a.px(12)).Clicked() {
					a.do("connect", nil)
				}
				if ui.Button(c, "Switch off").Padding(a.px(4), a.px(12)).TextColor(p.danger).Clicked() {
					a.do("disable_backend", map[string]any{"backend": current.ID})
				}
			} else if ui.PrimaryButton(c, "Switch on").Padding(a.px(4), a.px(12)).Clicked() {
				a.do("enable_backend", map[string]any{"backend": current.ID})
			}
		})
	})
	if f.err != "" {
		ui.Text(c, f.err).FontSize(a.size(12.5)).TextColor(p.danger)
	}
	if f.serverNote != "" {
		ui.Text(c, f.serverNote).FontSize(a.size(12)).TextColor(p.faint)
	}
	if current.Network {
		ui.Row(c).Gap(a.px(8)).Children(func() {
			if ui.Button(c, "Forget credential").Padding(a.px(5), a.px(12)).Clicked() {
				a.ask("Forget the credential?", "The saved "+strings.ToLower(current.CredentialLabel)+" for "+current.Name+" is removed from this Mac.", "Forget", true, func() {
					f.credential = ""
					a.do("clear_credential", nil)
				})
			}
			if s.ManagedLabel != nil {
				if ui.Button(c, "Stop "+*s.ManagedLabel).Padding(a.px(5), a.px(12)).Clicked() {
					a.do("stop_managed_server", nil)
				}
			}
		})
	}
}

func addressHint(s bridge.Settings, id string) string {
	switch {
	case s.ManagedLabel != nil:
		return "Local: Van-Goal starts " + *s.ManagedLabel + " for you."
	case id == "openclaw":
		return "A full ws:// or wss:// URL overrides the port."
	case id == "xuexiaomi":
		return "The gateway’s https:// address."
	}
	return "A server you already run."
}

func credentialHint(s bridge.Settings, id string) string {
	switch {
	case s.CredentialChars > 0:
		return fmt.Sprintf("%d characters saved.", s.CredentialChars)
	case s.DeviceTokenChars > 0:
		return "Connects with a paired device token."
	case id == "hermes":
		return "Found automatically for a local Hermes."
	}
	return "Nothing saved."
}

func (a *app) appearanceSettings(c *ui.Context) {
	p := a.p
	s := a.settings
	a.sectionTitle(c, "Appearance", "")
	a.group(c, "THEME", func() {
		ui.Row(c).Gap(a.px(10)).Padding(a.px(14)).Children(func() {
			for _, mode := range s.Appearances {
				mode := mode
				chosen := s.Appearance == mode.ID
				card := ui.ButtonBase(c.Key(mode.ID)).Column().AlignItems(ui.Stretch).Grow(1).Basis(0).MinWidth(0).Gap(a.px(8)).Padding(a.px(8)).Radius(a.px(12)).Border(1.5, p.line.Alpha(1.6)).Label(mode.Name)
				if chosen {
					card.Border(1.5, p.accent)
				} else if card.Hovered() {
					card.Background(p.raised)
				}
				card.Children(func() {
					a.themePreview(c, mode.ID)
					ui.Text(c, mode.Name).FontSize(a.size(13)).FontWeight(map[bool]int{true: 600, false: 450}[chosen]).TextColor(p.text).TextAlign(ui.Center)
				})
				if card.Clicked() {
					a.commit(map[string]any{"appearance": mode.ID})
				}
			}
		})
	})
	a.group(c, "TEXT SIZE", func() {
		ui.Column(c).Padding(a.px(14)).Gap(a.px(10)).Children(func() {
			index := 0
			labels := make([]string, len(s.FontSizes))
			for i, size := range s.FontSizes {
				labels[i] = size.Name
				if size.ID == s.FontSize {
					index = i
				}
			}
			chosen := index
			if ui.Segmented(c, &chosen, labels...).Label("Text size").Changed() && chosen != index {
				a.commit(map[string]any{"font_size": s.FontSizes[chosen].ID})
			}
			ui.Text(c, "Scales the transcript, the list and the composer.").FontSize(a.size(12)).TextColor(p.faint)
		})
	})
	a.group(c, "TRANSCRIPT", func() {
		a.field(c, "Show tool calls", "Off shows the replies alone.", func() {
			ui.Spacer(c)
			on := s.ShowToolCalls
			if ui.Switch(c, &on).Label("Show tool calls").Changed() {
				a.commit(map[string]any{"show_tool_calls": on})
			}
		})
	})
}

func (a *app) themePreview(c *ui.Context, mode string) {
	light, dark := lightPalette(), darkPalette()
	draw := func(pl palette) {
		ui.Column(c).Grow(1).Basis(0).Background(pl.bg).Padding(a.px(8)).Gap(a.px(5)).Children(func() {
			ui.Box(c).Height(a.px(6)).Width(a.px(40)).Radius(a.px(3)).Background(pl.muted.Alpha(0.5))
			ui.Box(c).Height(a.px(6)).Radius(a.px(3)).Background(pl.muted.Alpha(0.3))
			ui.Row(c).Children(func() {
				ui.Spacer(c)
				ui.Box(c).Height(a.px(10)).Width(a.px(34)).Radius(a.px(5)).Background(pl.userBg)
			})
			ui.Box(c).Height(a.px(6)).Width(a.px(28)).Radius(a.px(3)).Background(pl.accent)
		})
	}
	ui.Row(c).Height(a.px(70)).Radius(a.px(8)).Clip().Border(1, a.p.line).Children(func() {
		switch mode {
		case "light":
			draw(light)
		case "dark":
			draw(dark)
		default:
			draw(light)
			draw(dark)
		}
	})
}

func (a *app) advancedSettings(c *ui.Context) {
	p := a.p
	s := a.settings
	a.sectionTitle(c, "Advanced", "")
	a.group(c, "DIAGNOSTICS", func() {
		a.field(c, "Debug log", s.LogPath, func() {
			ui.Spacer(c)
			if ui.Button(c, "Clear").Padding(a.px(4), a.px(10)).Clicked() {
				a.do("clear_log", nil)
				a.notice = "Log cleared."
			}
			on := s.DebugLogging
			if ui.Switch(c, &on).Label("Debug log").Changed() {
				a.commit(map[string]any{"debug_logging": on})
			}
		})
	})
	a.group(c, "LOCAL DATA", func() {
		a.field(c, "Session cache", a.state.CacheSummary, func() {
			ui.Spacer(c)
			if ui.Button(c, "Clear cache").Padding(a.px(4), a.px(10)).TextColor(p.danger).Clicked() {
				a.ask("Clear the local cache?", "Saved transcripts and the session list on this Mac are removed. Sessions on the backend are not touched.", "Clear", true, func() {
					a.do("clear_cache", nil)
				})
			}
		})
	})
	ui.Text(c, "Van-Goal "+version+" · built with MyGo").FontSize(a.size(11.5)).TextColor(p.faint)
}

// ------------------------------------------------------------ confirmation

func (a *app) ask(title, body, action string, destructive bool, run func()) {
	a.confirm = &confirmation{title: title, body: body, action: action, destructive: destructive, run: run}
}

func (a *app) confirmDialog(c *ui.Context) {
	if a.confirm == nil {
		return
	}
	p := a.p
	q := a.confirm
	open := true
	ui.DialogBase(c, &open, func(backdrop, panel ui.Element) {
		backdrop.Background(p.overlayBg)
		panel.Width(a.px(380)).Padding(a.px(22)).Gap(a.px(10)).Radius(a.px(16)).Background(p.bg).Border(1, p.line).Shadow(0, 24, 64, 0, p.shadow.Alpha(2))
		panel.Transition(ui.ElementTransition{Enter: &ui.Motion{Opacity: 0, Scale: 0.96}})
		ui.Text(c, q.title).FontSize(a.size(15.5)).FontWeight(650).TextColor(p.text)
		ui.Text(c, q.body).FontSize(a.size(13)).LineHeight(1.5).TextColor(p.muted)
		ui.Row(c).Gap(a.px(8)).Justify(ui.End).Margin(a.px(10), 0, 0, 0).Children(func() {
			if ui.Button(c, "Cancel").Padding(a.px(6), a.px(14)).Clicked() {
				a.confirm = nil
			}
			yes := ui.PrimaryButton(c, q.action).Padding(a.px(6), a.px(14)).AutoFocus()
			if q.destructive {
				yes.Background(p.danger)
			}
			if yes.Clicked() {
				q.run()
				a.confirm = nil
			}
		})
	})
	if !open {
		a.confirm = nil
	}
}

// ------------------------------------------------------------------ toasts

func (a *app) toasts(c *ui.Context) {
	if a.notice != "" {
		c.Toast(a.notice)
		a.notice = ""
	}
}
