package bridge

import (
	"os"
	"path/filepath"
	"testing"
)

// The library is loaded once per process, with a throwaway app-data directory
// so a test never reads or rewrites the developer's settings.
func TestMain(m *testing.M) {
	dir, _ := filepath.Abs("../../target/debug")
	if err := Load(dir); err != nil {
		println("skipping: build the library first (cargo build -p van-goal-desktop):", err.Error())
		os.Exit(0)
	}
	data, _ := os.MkdirTemp("", "van-goal-go-test-")
	if !SetDataDir(data) {
		panic("app-data directory was already resolved")
	}
	code := m.Run()
	os.RemoveAll(data)
	os.Exit(code)
}

func TestStartReturnsTheFirstFrame(t *testing.T) {
	var first Update
	if err := Command("start", nil, &first); err != nil {
		t.Fatal(err)
	}
	if first.State == nil || first.Settings == nil || first.Messages == nil {
		t.Fatalf("start left out part of the first frame: %+v", first)
	}
	if len(first.Settings.Backends) != 8 {
		t.Errorf("%d backends offered, want 8", len(first.Settings.Backends))
	}
}

func TestMarkdownComesBackAsBlocks(t *testing.T) {
	var reply struct {
		Blocks []map[string]any `json:"blocks"`
	}
	if err := Command("markdown", map[string]any{"text": "# 标题\n\n正文"}, &reply); err != nil {
		t.Fatal(err)
	}
	if len(reply.Blocks) != 2 || reply.Blocks[0]["kind"] != "heading" {
		t.Errorf("blocks %+v", reply.Blocks)
	}
}

func TestAnUnknownCommandIsAnError(t *testing.T) {
	if err := Command("no-such-command", nil, nil); err == nil {
		t.Error("an unknown command succeeded")
	}
}

func TestABadSettingIsRefusedWithAReason(t *testing.T) {
	err := Command("settings", map[string]any{"port": "99999"}, nil)
	if err == nil {
		t.Fatal("port 99999 was accepted")
	}
}
