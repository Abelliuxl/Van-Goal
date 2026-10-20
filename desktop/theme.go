package main

import (
	"github.com/egoist/mygo/ui"
	"github.com/egoist/mygo/ui/markdown"
)

// palette is the app's own colors on top of MyGo's theme. Calm neutrals with
// a single accent; depth comes from soft shadows and tints, not from lines.
type palette struct {
	dark bool

	bg        ui.Color // the conversation
	sidebar   ui.Color // the session list, a step darker
	surface   ui.Color // cards: the composer, popovers, dialogs
	raised    ui.Color // a hovered row, a pressed chip
	selected  ui.Color // the session on screen
	line      ui.Color // hairlines where one is needed
	text      ui.Color
	muted     ui.Color
	faint     ui.Color
	accent    ui.Color
	accentBg  ui.Color // tinted accent, for selection and pills
	userBg    ui.Color // the user's bubble
	codeBg    ui.Color
	danger    ui.Color
	warn      ui.Color
	ok        ui.Color
	shadow    ui.Color
	overlayBg ui.Color
}

func lightPalette() palette {
	return palette{
		bg:        ui.Hex("#fbfbfa"),
		sidebar:   ui.Hex("#f3f2ef"),
		surface:   ui.Hex("#ffffff"),
		raised:    ui.Hex("#ebe9e5"),
		selected:  ui.Hex("#e4e2dd"),
		line:      ui.RGBA(30, 28, 24, 0.08),
		text:      ui.Hex("#1f1e1c"),
		muted:     ui.Hex("#6b6862"),
		faint:     ui.Hex("#9c9891"),
		accent:    ui.Hex("#c2603c"),
		accentBg:  ui.RGBA(194, 96, 60, 0.12),
		userBg:    ui.Hex("#efede8"),
		codeBg:    ui.Hex("#f4f3f0"),
		danger:    ui.Hex("#c8372d"),
		warn:      ui.Hex("#b7791f"),
		ok:        ui.Hex("#2f9e5b"),
		shadow:    ui.RGBA(40, 32, 20, 0.10),
		overlayBg: ui.RGBA(20, 18, 14, 0.28),
	}
}

func darkPalette() palette {
	return palette{
		dark:      true,
		bg:        ui.Hex("#1c1b19"),
		sidebar:   ui.Hex("#161513"),
		surface:   ui.Hex("#262522"),
		raised:    ui.Hex("#2c2b28"),
		selected:  ui.Hex("#33312d"),
		line:      ui.RGBA(255, 250, 240, 0.08),
		text:      ui.Hex("#ecebe7"),
		muted:     ui.Hex("#a29f98"),
		faint:     ui.Hex("#76736d"),
		accent:    ui.Hex("#e07a52"),
		accentBg:  ui.RGBA(224, 122, 82, 0.16),
		userBg:    ui.Hex("#2e2d2a"),
		codeBg:    ui.Hex("#232220"),
		danger:    ui.Hex("#f0685c"),
		warn:      ui.Hex("#e0a443"),
		ok:        ui.Hex("#4cc27f"),
		shadow:    ui.RGBA(0, 0, 0, 0.35),
		overlayBg: ui.RGBA(0, 0, 0, 0.45),
	}
}

// applyTheme decides light or dark from the setting (or the system), and
// hands MyGo's widgets a theme in the same colors, so a stock button sits in
// the design instead of next to it.
func (a *app) applyTheme(c *ui.Context) {
	dark := c.Theme().Dark
	switch a.settings.Appearance {
	case "light":
		dark = false
	case "dark":
		dark = true
	}
	if dark {
		a.p = darkPalette()
	} else {
		a.p = lightPalette()
	}
	p := a.p

	base := ui.LightTheme()
	if dark {
		base = ui.DarkTheme()
	}
	t := *base
	t.Background = p.bg
	t.Surface = p.raised
	t.SurfaceHover = p.selected
	t.SurfacePressed = p.selected.Mix(p.text, 0.08)
	t.Border = p.line.Alpha(2.2)
	t.Text = p.text
	t.TextMuted = p.muted
	t.Accent = p.accent
	t.AccentHover = p.accent.Mix(p.text, 0.12)
	t.AccentPressed = p.accent.Mix(p.text, 0.22)
	t.AccentText = ui.Hex("#ffffff")
	t.Danger, t.Warning, t.Success = p.danger, p.warn, p.ok
	t.Selection = p.accent.Alpha(0.24)
	t.Focus = p.accent.Alpha(0.5)
	t.Radius = 8
	t.ScrollbarWidth = 6
	textScale, layoutScale := a.scaleFactors()
	t.FontSize = 14 * textScale
	c.SetTheme(&t)
	a.scale, a.pxScale = textScale, layoutScale

	a.md.Theme = &markdown.Theme{
		FontSize:       15 * textScale,
		LineHeight:     1.62,
		Text:           p.text,
		Muted:          p.muted,
		Link:           p.accent,
		Strong:         p.text,
		CodeBackground: p.codeBg,
		CodeSize:       0.88,
		BlockSpacing:   0.8,
		ItemSpacing:    0.3,
		QuoteBar:       p.line.Alpha(3),
		QuoteText:      p.muted,
		Rule:           p.line.Alpha(1.6),
	}
}

// scaleFactors returns the multiplier for fonts (the text-size setting times
// the interface zoom) and for layout (the zoom alone). They differ on purpose:
// text size grows type inside chrome that stays put, while zoom grows the
// whole window together.
func (a *app) scaleFactors() (text, layout float32) {
	text = a.settings.FontScale
	if text <= 0 {
		text = 1
	}
	layout = a.settings.UIScale
	if layout <= 0 {
		layout = 1
	}
	return text * layout, layout
}

// size scales a design font size by the text size the user chose and the
// interface zoom.
func (a *app) size(v float32) float32 { return v * a.scale }

// px scales a design layout dimension by the interface zoom alone, so the
// text-size setting can grow type without pushing content out of the chrome,
// while Zoom In grows the whole window together.
func (a *app) px(v float32) float32 { return v * a.pxScale }
