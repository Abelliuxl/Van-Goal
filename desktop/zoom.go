package main

// zoomLevels is the ladder View > Zoom In and Zoom Out walk. The ends match
// the range the core stores (UI_SCALE_MIN..UI_SCALE_MAX), so a step never
// reaches a value the core would clamp behind the window's back.
var zoomLevels = []float32{0.8, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2}

// zoomIn enlarges the interface one step, as ⌘+ does in macOS apps.
func (a *app) zoomIn() { a.setZoom(nextZoom(a.settings.UIScale, 1)) }

// zoomOut shrinks it one step.
func (a *app) zoomOut() { a.setZoom(nextZoom(a.settings.UIScale, -1)) }

// actualSize returns to 100%.
func (a *app) actualSize() { a.setZoom(1) }

// nextZoom is the nearest rung above (direction > 0) or below the current
// scale. A scale that sits between rungs — a file written by another build —
// still moves in the direction asked for instead of sticking.
func nextZoom(current float32, direction int) float32 {
	if current <= 0 {
		current = 1
	}
	if direction > 0 {
		for _, level := range zoomLevels {
			if level > current+0.001 {
				return level
			}
		}
		return zoomLevels[len(zoomLevels)-1]
	}
	for i := len(zoomLevels) - 1; i >= 0; i-- {
		if zoomLevels[i] < current-0.001 {
			return zoomLevels[i]
		}
	}
	return zoomLevels[0]
}

// setZoom stores the new scale in the core, which is also what keeps it
// across restarts; the reply carries the clamped value back.
func (a *app) setZoom(scale float32) {
	if a.settings.UIScale == scale {
		return
	}
	_ = a.setting(map[string]any{"ui_scale": scale})
}
