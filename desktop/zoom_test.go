package main

import (
	"math"
	"testing"

	"github.com/egoist/mygo/ui"
)

func TestTheZoomLadderMovesOneStepAndStopsAtItsEnds(t *testing.T) {
	cases := []struct {
		from float32
		dir  int
		want float32
	}{
		{1, 1, 1.1},
		{1, -1, 0.9},
		{0.8, -1, 0.8},  // already the smallest
		{2, 1, 2},       // already the largest
		{1.17, 1, 1.25}, // a value from another build still moves
		{1.17, -1, 1.1}, //
	}
	for _, c := range cases {
		if got := nextZoom(c.from, c.dir); got != c.want {
			t.Errorf("nextZoom(%v, %d) = %v, want %v", c.from, c.dir, got, c.want)
		}
	}
}

func TestZoomScalesLayoutWhileTextSizeOnlyScalesText(t *testing.T) {
	a := newApp()
	a.settings.FontScale = 1.15
	a.settings.UIScale = 1.25

	text, layout := a.scaleFactors()
	if layout != 1.25 {
		t.Fatalf("layout scale = %v, want the zoom alone (1.25)", layout)
	}
	if math.Abs(float64(text-1.15*1.25)) > 1e-6 {
		t.Fatalf("text scale = %v, want text size times zoom (1.4375)", text)
	}

	// A settings file from before the field existed, or a refused write,
	// leaves a zero behind; neither should blank the interface.
	a.settings.FontScale = 0
	a.settings.UIScale = 0
	if text, layout := a.scaleFactors(); text != 1 || layout != 1 {
		t.Fatalf("zero scales became text=%v layout=%v, want 1 and 1", text, layout)
	}
}

// The ladder is only useful if the core stores the step and the window still
// draws at it; this runs both, against the real core in a throwaway directory.
func TestZoomReachesTheCoreAndTheWindowStillDraws(t *testing.T) {
	a := started(t)

	if err := a.setting(map[string]any{"ui_scale": 1.25}); err != nil {
		t.Fatalf("zoom: %v", err)
	}
	if a.settings.UIScale != 1.25 {
		t.Fatalf("the core reported zoom %v, want 1.25", a.settings.UIScale)
	}

	// Past either end the core clamps, and the reply carries the value back.
	if err := a.setting(map[string]any{"ui_scale": 9}); err != nil {
		t.Fatalf("zoom past the top: %v", err)
	}
	if a.settings.UIScale != 2 {
		t.Fatalf("zoom past the top was stored as %v, want 2", a.settings.UIScale)
	}
	if err := a.setting(map[string]any{"ui_scale": 1.25}); err != nil {
		t.Fatalf("zoom back: %v", err)
	}

	tt := ui.NewTester(a.view, 1180, 780)
	if !tt.HasText("Chats") || !tt.HasText("New chat") {
		t.Fatalf("the sidebar is missing at 125%%: %q", tt.Texts())
	}
	snapshot(t, tt, "welcome-zoom-125")

	// At the top of the ladder the settings panel is clamped to the window,
	// so it cannot grow past what the user can see.
	if err := a.setting(map[string]any{"ui_scale": 2}); err != nil {
		t.Fatalf("zoom to the top: %v", err)
	}
	if tt := ui.NewTester(a.view, 1180, 780); !tt.HasText("Chats") {
		t.Fatalf("the sidebar is missing at 200%%: %q", tt.Texts())
	} else {
		snapshot(t, tt, "welcome-zoom-200")
	}
	a.openSettings()
	tt = ui.NewTester(a.view, 1180, 780)
	if !tt.HasText("Settings") {
		t.Fatalf("the settings panel did not open at 200%%: %q", tt.Texts())
	}
	snapshot(t, tt, "settings-zoom-200")
}
