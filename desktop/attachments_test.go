package main

import (
	"bytes"
	"encoding/base64"
	"image"
	"image/color"
	"image/png"
	"os"
	"testing"
	"time"

	"github.com/Abelliuxl/Van-Goal/desktop/bridge"
	"github.com/egoist/mygo/ui"
)

func TestMediaComposerAndReceivedFiles(t *testing.T) {
	a := started(t)
	img := image.NewRGBA(image.Rect(0, 0, 180, 100))
	for y := 0; y < 100; y++ {
		for x := 0; x < 180; x++ {
			img.Set(x, y, color.RGBA{R: 215, G: 80, B: 60, A: 255})
		}
	}
	var data bytes.Buffer
	if err := png.Encode(&data, img); err != nil {
		t.Fatal(err)
	}
	var reply struct {
		Path string `json:"path"`
	}
	if err := bridge.Command("paste_image", map[string]any{"data": base64.StdEncoding.EncodeToString(data.Bytes())}, &reply); err != nil {
		t.Fatal(err)
	}
	defer os.Remove(reply.Path)
	stored, err := os.ReadFile(reply.Path)
	if err != nil || !bytes.Equal(stored, data.Bytes()) {
		t.Fatal("pasted image was not preserved")
	}
	a.attachments = []string{reply.Path}
	a.draft = "Read this image and the document."
	a.messages = []bridge.Message{
		{ID: "user", Role: "user", Content: "Please send the report.", Timestamp: float64(time.Now().Unix())},
		{ID: "assistant", Role: "assistant", Attachments: []bridge.Attachment{{Name: "报告.pdf", Path: "/api/download?token=fixture", Remote: true}}},
	}
	tt := ui.NewTester(a.view, 1180, 780)
	deadline := time.Now().Add(3 * time.Second)
	for {
		loaded := false
		for _, p := range a.previews {
			p.mu.Lock()
			loaded = p.bitmap != nil
			p.mu.Unlock()
		}
		if loaded {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("composer image preview did not load")
		}
		time.Sleep(10 * time.Millisecond)
		tt.Frame()
	}
	tt.Frame()
	if !tt.HasText("Save 报告.pdf…") {
		t.Fatalf("attachment-only reply has no Save button: %q", tt.Texts())
	}
	snapshot(t, tt, "attachments-light")
	tt.SetDark(true)
	tt.Frame()
	snapshot(t, tt, "attachments-dark")
	// Invalid clipboard data must not be written as an image.
	if err := bridge.Command("paste_image", map[string]any{"data": base64.StdEncoding.EncodeToString([]byte("not an image"))}, nil); err == nil {
		t.Fatal("invalid image accepted")
	}
}
