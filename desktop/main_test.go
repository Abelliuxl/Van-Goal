package main

import (
	"image/png"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/egoist/mygo/ui"

	"github.com/Abelliuxl/Van-Goal/desktop/bridge"
)

// The view runs headless against the real Rust core, in a throwaway app-data
// directory: nothing here can touch the developer's settings or a backend.
func TestMain(m *testing.M) {
	dir, _ := filepath.Abs("../target/debug")
	if err := bridge.Load(dir); err != nil {
		println("skipping: build the library first (cargo build -p van-goal-desktop):", err.Error())
		os.Exit(0)
	}
	data, _ := os.MkdirTemp("", "van-goal-ui-test-")
	if !bridge.SetDataDir(data) {
		panic("app-data directory was already resolved")
	}
	code := m.Run()
	os.RemoveAll(data)
	os.Exit(code)
}

func started(t *testing.T) *app {
	t.Helper()
	a := newApp()
	var first bridge.Update
	if err := bridge.Command("start", nil, &first); err != nil {
		t.Fatal(err)
	}
	a.apply(first)
	// A fresh install has its default backend switched on; tests never
	// reach for a backend, so it is switched off.
	if err := bridge.Command("disconnect", nil, nil); err != nil {
		t.Fatal(err)
	}
	u, _ := bridge.Poll()
	a.apply(u)
	return a
}

// snapshot writes what the tester drew, for looking at by eye:
// VG_SNAPSHOTS=dir go test -run Snapshot
func snapshot(t *testing.T, tt *ui.Tester, name string) {
	dir := os.Getenv("VG_SNAPSHOTS")
	if dir == "" {
		return
	}
	// Let hovers and color transitions settle before the picture is taken.
	tt.Advance(time.Second)
	tt.Frame()
	f, err := os.Create(filepath.Join(dir, name+".png"))
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	if err := png.Encode(f, tt.Image()); err != nil {
		t.Fatal(err)
	}
}

func TestTheFirstFrameShowsTheWelcome(t *testing.T) {
	a := started(t)
	tt := ui.NewTester(a.view, 1180, 780)
	if !tt.HasText("Chats") || !tt.HasText("New chat") {
		t.Fatalf("sidebar missing: %q", tt.Texts())
	}
	if !tt.HasText(a.backendName() + " is switched off") {
		t.Errorf("a switched-off backend should say so: %q", tt.Texts())
	}
	snapshot(t, tt, "welcome-light")
	tt.SetDark(true)
	tt.Frame()
	snapshot(t, tt, "welcome-dark")
}

func TestTheSidebarHidesAndComesBack(t *testing.T) {
	a := started(t)
	tt := ui.NewTester(a.view, 1000, 700)
	tt.Key(ui.Cmd, ui.KeyB)
	tt.Frame()
	if a.sidebarOpen || tt.HasText("Chats") {
		t.Fatalf("%s did not hide the sidebar", shortcut("B"))
	}
	tt.Key(ui.Cmd, ui.KeyB)
	tt.Frame()
	if !a.sidebarOpen || !tt.HasText("Chats") {
		t.Fatalf("%s did not bring the sidebar back", shortcut("B"))
	}
}

// A window with a hidden title bar draws its controls at the top-right on
// Windows. The header has to stop before them: ending at its own padding
// would put the refresh button under Close.
func TestTheHeaderKeepsClearOfTheWindowControls(t *testing.T) {
	a := started(t)
	tt := ui.NewTester(a.view, 1180, 780)
	const controls = 138 // minimize, maximize, close at 46 DIPs each
	tt.SetTitleBar(ui.TitleBar{Height: 32, Right: controls})
	rect, ok := tt.Find("Refresh  " + shortcut("R"))
	if !ok {
		t.Fatalf("refresh button missing: %q", tt.Texts())
	}
	if right := rect.X + rect.W; right > 1180-controls {
		t.Errorf("refresh ends at %.0f, under the window controls starting at %d", right, 1180-controls)
	}
}

func TestSettingsOpenOnEverySection(t *testing.T) {
	a := started(t)
	tt := ui.NewTester(a.view, 1180, 780)
	tt.Key(ui.Cmd, ui.KeyComma)
	tt.Frame()
	if !a.settingsOpen || !tt.HasText("雪小米") || !tt.HasText("BACKEND") {
		t.Fatalf("settings did not open on the backends: %q", tt.Texts())
	}
	snapshot(t, tt, "settings-agent")
	for _, section := range []string{"Appearance", "Advanced"} {
		if err := tt.Click(section); err != nil {
			t.Fatal(err)
		}
		tt.Frame()
		snapshot(t, tt, "settings-"+strings.ToLower(section))
	}
	if !tt.HasText("Van-Goal " + version + " · built with MyGo") {
		t.Errorf("advanced section missing: %q", tt.Texts())
	}
}

// A transcript as the core reports it: markdown, a tool group, a streaming
// placeholder, a failure.
func TestATranscriptDraws(t *testing.T) {
	a := started(t)
	done := 1.0e9 + 12
	a.apply(bridge.Update{
		State: &bridge.State{
			Backend:    bridge.Backend{ID: "xuexiaomi", Name: "雪小米", Enabled: true, Caps: bridge.Caps{History: true}},
			Connection: bridge.Connection{State: "connected", Label: "Connected"},
			Sending:    true,
			Sessions:   []bridge.Session{{ID: "s1", Title: "整理实验记录"}},
			Selected:   &bridge.Session{ID: "s1", Title: "整理实验记录"},
			Context:    bridge.Context{Used: 30_000, Max: 258_000, Ratio: 0.12},
			Queue:      []bridge.Queued{{ID: "q1", Text: "然后把结论发到飞书"}},
		},
		Messages: &[]bridge.Message{
			{ID: "u1", Role: "user", Content: "帮我看看 `G5E` 的流式结果", Timestamp: 1e9},
			{ID: "a1", Role: "assistant", Content: "## 结论\n\n- **荧光强度**提高了 *两倍*\n- 对照组正常\n\n```go\nfmt.Println(\"ok\")\n```",
				Timestamp: 1e9, CompletedAt: &done,
				Tools: []bridge.Tool{{ID: "t1", Name: "read_file", Status: "completed", Detail: "G5E/2026-10-19.md"}}},
			{ID: "u2", Role: "user", Content: "再写一份周报"},
			{ID: "a2", Role: "assistant", Streaming: true},
		},
	})
	a.settings.ShowToolCalls = true
	tt := ui.NewTester(a.view, 1180, 780)
	tt.Frame()
	for _, want := range []string{"整理实验记录", "1 tool call", "结论", "Thinking", "然后把结论发到飞书"} {
		if !tt.HasText(want) {
			t.Errorf("%q not drawn; texts %q", want, tt.Texts())
		}
	}
	snapshot(t, tt, "transcript")
	tt.SetDark(true)
	tt.Frame()
	snapshot(t, tt, "transcript-dark")

	if err := tt.Click("1 tool call"); err != nil {
		t.Fatal(err)
	}
	tt.Frame()
	if !tt.HasText("read_file") {
		t.Error("opening the tool group did not list its calls")
	}
}

// The composer gives a prompt the core refused back, in front of what was
// typed since.
func TestAPromptThatCameBackIsNotLost(t *testing.T) {
	a := newApp()
	a.draft = "typed meanwhile"
	a.apply(bridge.Update{Composer: []bridge.ComposerEvent{{Action: "restore", Text: "the prompt", Attachments: []string{"/a.txt"}}}})
	if a.draft != "the prompt\n\ntyped meanwhile" || len(a.attachments) != 1 {
		t.Fatalf("draft %q attachments %v", a.draft, a.attachments)
	}
	a.apply(bridge.Update{Composer: []bridge.ComposerEvent{{Action: "clear"}}})
	if a.draft != "" || a.attachments != nil {
		t.Fatalf("clear left %q %v", a.draft, a.attachments)
	}
}

func TestDeltasUpdateTheMessageInPlace(t *testing.T) {
	a := newApp()
	a.apply(bridge.Update{Messages: &[]bridge.Message{{ID: "m", Role: "assistant", Streaming: true}}})
	a.apply(bridge.Update{Deltas: []bridge.Delta{{ID: "m", Content: "雪落"}, {ID: "gone", Content: "x"}}})
	if a.messages[0].Content != "雪落" {
		t.Fatalf("content %q", a.messages[0].Content)
	}
}

func TestSendingAsksTheCoreAndKeepsTheDraftUntilItAnswers(t *testing.T) {
	a := started(t)
	a.draft = "hello"
	a.submit(a.draft)
	// The backend is switched off in a fresh install: the core refuses with
	// a reason, and the draft is still there.
	if a.draft != "hello" {
		t.Errorf("the draft was dropped before the core took it")
	}
	u, err := bridge.Poll()
	if err != nil {
		t.Fatal(err)
	}
	a.apply(u)
	if a.state.Error == nil || !strings.Contains(*a.state.Error, "switched off") {
		t.Errorf("no reason given: %+v", a.state.Error)
	}
}

func TestHelpers(t *testing.T) {
	if compact(1_048_576) != "1M" || compact(36_393) != "36.4k" || compact(12) != "12" {
		t.Error(compact(1_048_576), compact(36_393))
	}
	if duration(5) != "5s" || duration(75) != "1m 15s" || duration(3700) != "1h 1m" {
		t.Error(duration(75))
	}
	if got := mergePaths([]string{"a"}, []string{"a", "b"}); len(got) != 2 {
		t.Error(got)
	}
}
