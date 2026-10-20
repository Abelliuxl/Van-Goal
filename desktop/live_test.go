package main

import (
	"os"
	"strings"
	"testing"
	"time"

	"github.com/egoist/mygo/ui"

	"github.com/Abelliuxl/Van-Goal/desktop/bridge"
)

// TestLiveXueXiaoMi drives the window, headless, against a real 雪小米
// gateway: connect, list, send, stream, stop. It creates a session there, so
// it runs only when asked:
//
//	VG_XXM_URL=https://host:port/ VG_XXM_TOKEN_FILE=path go test -run Live
func TestLiveXueXiaoMi(t *testing.T) {
	url, tokenFile := os.Getenv("VG_XXM_URL"), os.Getenv("VG_XXM_TOKEN_FILE")
	if url == "" || tokenFile == "" {
		t.Skip("VG_XXM_URL and VG_XXM_TOKEN_FILE not set")
	}
	raw, err := os.ReadFile(tokenFile)
	if err != nil {
		t.Fatal(err)
	}
	a := started(t)
	tt := ui.NewTester(a.view, 1180, 780)
	// What the window does on its own: poll the core and redraw.
	pump := func() {
		for bridge.Wait(50) {
			u, err := bridge.Poll()
			if err != nil {
				t.Fatal(err)
			}
			a.apply(u)
		}
		tt.Advance(100 * time.Millisecond)
		tt.Frame()
	}
	until := func(what string, limit time.Duration, ok func() bool) {
		t.Helper()
		deadline := time.Now().Add(limit)
		for !ok() {
			if time.Now().After(deadline) {
				t.Fatalf("timed out waiting for %s; texts %q", what, tt.Texts())
			}
			pump()
		}
		t.Logf("✓ %s", what)
	}

	a.do("enable_backend", map[string]any{"backend": "xuexiaomi"})
	if err := a.setting(map[string]any{"host": url, "credential": strings.TrimSpace(string(raw))}); err != nil {
		t.Fatal(err)
	}
	a.do("connect", nil)
	until("connected", 30*time.Second, func() bool { return a.connected() && tt.HasText("Connected") })
	until("sessions in the sidebar", 30*time.Second, func() bool { return len(a.state.Sessions) > 0 })
	snapshot(t, tt, "live-connected")

	// Type in the composer and press Enter, as a user would.
	a.newChat()
	pump()
	tt.Type("这是 Van-Goal 桌面界面自检：不调用工具，用一段简短的 Markdown 列表回复三种雪的形态。")
	tt.Key(0, ui.KeyEnter)
	until("prompt shown and composer cleared", 5*time.Second, func() bool {
		return len(a.messages) >= 2 && a.state.Sending && a.draft == ""
	})
	until("reply streaming in", 240*time.Second, func() bool {
		m := a.messages[len(a.messages)-1]
		return m.Streaming && len([]rune(m.Content)) > 6
	})
	snapshot(t, tt, "live-streaming")
	until("reply finished", 240*time.Second, func() bool { return !a.state.Sending })
	last := a.messages[len(a.messages)-1]
	if last.Role != "assistant" || strings.TrimSpace(last.Content) == "" || strings.HasPrefix(last.Content, "Error:") {
		t.Fatalf("unexpected reply %+v (error %v)", last, a.state.Error)
	}
	until("the chat is selected and named", 30*time.Second, func() bool {
		return a.state.Selected != nil && tt.HasText(a.state.Selected.Title)
	})
	snapshot(t, tt, "live-done")

	// A long reply, stopped from the composer.
	before := len(a.messages)
	tt.Type("界面自检：写一首 30 行关于雪的诗，不调用工具。")
	tt.Key(0, ui.KeyEnter)
	until("second prompt sent", 10*time.Second, func() bool {
		return len(a.messages) > before && a.state.Sending && a.draft == ""
	})
	// A gateway may hold the whole reply back and send it at once, so the
	// stop is pressed as soon as any text is in, or skipped if it is done.
	until("second reply arriving", 240*time.Second, func() bool {
		m := a.messages[len(a.messages)-1]
		return len(a.messages) > before+1 && m.Role == "assistant" && len([]rune(m.Content)) > 0
	})
	if a.state.Sending {
		if err := tt.Click("Stop"); err != nil {
			t.Fatal(err)
		}
	}
	until("turn over, text kept", 30*time.Second, func() bool {
		m := a.messages[len(a.messages)-1]
		return !a.state.Sending && m.Role == "assistant" && !strings.HasPrefix(m.Content, "Error:") && m.Content != ""
	})
	if n := len(a.messages); n != before+2 {
		t.Errorf("%d messages after the second turn, want %d", n, before+2)
	}
	snapshot(t, tt, "live-stopped")
	a.do("disconnect", nil)
	pump()
}
