package main

import (
	"bytes"
	"encoding/base64"
	"fmt"
	"image"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync"

	"github.com/Abelliuxl/Van-Goal/desktop/bridge"
	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"
)

type attachmentPreview struct {
	mu      sync.Mutex
	bitmap  *ui.Bitmap
	err     error
	loading bool
}

func imageFilename(name string) bool {
	switch strings.ToLower(filepath.Ext(name)) {
	case ".png", ".jpg", ".jpeg", ".gif", ".webp":
		return true
	}
	return false
}

// File and image reads run off the drawing thread. Only the bridge owns
// gateway credentials; the window never builds an authenticated request.
func (a *app) imagePreview(c *ui.Context, path, name string, remote bool, width float32) {
	if !imageFilename(name) {
		return
	}
	key := fmt.Sprintf("%s:%s:%t:%s", a.state.Backend.ID, a.settings.URL, remote, path)
	entry := a.previews[key]
	if entry == nil {
		entry = &attachmentPreview{loading: true}
		a.previews[key] = entry
		services := c.Services()
		go func() {
			var data []byte
			var err error
			if remote {
				var reply struct {
					Data string `json:"data"`
				}
				err = bridge.Command("read_attachment", map[string]any{"source": path}, &reply)
				if err == nil {
					data, err = base64.StdEncoding.DecodeString(reply.Data)
				}
			} else {
				var f *os.File
				f, err = os.Open(path)
				if err == nil {
					data, err = io.ReadAll(io.LimitReader(f, 8*1024*1024+1))
					f.Close()
				}
			}
			var bitmap *ui.Bitmap
			if err == nil && len(data) > 8*1024*1024 {
				err = fmt.Errorf("Image exceeds 8 MB")
			}
			if err == nil {
				// Reject decompression bombs before decoding a bitmap.
				var config image.Config
				config, _, err = image.DecodeConfig(bytes.NewReader(data))
				if err == nil && (config.Width > 12000 || config.Height > 12000 || int64(config.Width)*int64(config.Height) > 40_000_000) {
					err = fmt.Errorf("Image is too large to preview")
				}
				if err == nil {
					bitmap, err = ui.DecodeBitmap(data)
				}
			}
			entry.mu.Lock()
			entry.bitmap, entry.err, entry.loading = bitmap, err, false
			entry.mu.Unlock()
			services.Invalidate()
		}()
	}
	entry.mu.Lock()
	defer entry.mu.Unlock()
	if entry.bitmap != nil {
		w, h := entry.bitmap.Size()
		displayW := min(width, a.px(float32(w)))
		displayH := displayW * float32(h) / float32(w)
		if displayH > a.px(220) {
			displayW *= a.px(220) / displayH
			displayH = a.px(220)
		}
		ui.Image(c, entry.bitmap).Size(displayW, displayH).Radius(a.px(10)).Label(name)
	} else if entry.loading {
		ui.Text(c, "Loading image…").FontSize(a.size(12)).TextColor(a.p.muted)
	} else if entry.err != nil {
		ui.Text(c, "Preview unavailable").FontSize(a.size(12)).TextColor(a.p.muted).Tooltip(entry.err.Error())
		if ui.Button(c, "Retry preview").FontSize(a.size(12)).Padding(a.px(4), a.px(8)).Clicked() {
			delete(a.previews, key)
		}
	}
}

func (a *app) messageAttachment(c *ui.Context, at bridge.Attachment) {
	ui.Column(c.Key("attachment:" + at.Path)).Gap(a.px(6)).AlignItems(ui.Start).Children(func() {
		a.imagePreview(c, at.Path, at.Name, at.Remote, a.px(320))
		if !at.Remote {
			b := ui.Button(c, at.Name).FontSize(a.size(12.5)).Radius(a.px(8)).Padding(a.px(5), a.px(10)).Tooltip("Open " + at.Name)
			if b.Clicked() {
				if err := mygo.Shell.OpenPath(at.Path); err != nil {
					a.notice = err.Error()
				}
			}
			return
		}
		label := "Save " + at.Name + "…"
		busy := a.downloads[at.Path]
		if busy {
			label = "Saving " + at.Name + "…"
		}
		if ui.Button(c, label).Disabled(busy).FontSize(a.size(12.5)).Radius(a.px(8)).Padding(a.px(5), a.px(10)).Clicked() {
			a.saveAttachment(at)
		}
	})
}

func (a *app) saveAttachment(at bridge.Attachment) {
	a.downloads[at.Path] = true
	go func() {
		home, err := os.UserHomeDir()
		var path string
		if err == nil {
			path, err = mygo.Dialog.Save(mygo.SaveDialogOptions{
				Parent: a.win, Title: "Save attachment",
				DefaultPath:       filepath.Join(home, "Downloads", filepath.Base(at.Name)),
				CreateDirectories: true,
			})
		}
		if err == nil && path != "" {
			err = bridge.Command("download_attachment", map[string]any{"source": at.Path, "destination": path}, nil)
		}
		a.win.Update(func() {
			delete(a.downloads, at.Path)
			if err != nil {
				a.notice = err.Error()
			}
		})
	}()
}

func (a *app) pasteAttachment(text string) bool {
	if files, err := mygo.Clipboard.ReadFiles(); err == nil && len(files) > 0 {
		a.attachments = mergePaths(a.attachments, files)
		return true
	}
	if data := mygo.Clipboard.ReadImage(); len(data) > 0 {
		if len(data) > 8*1024*1024 {
			a.notice = "Image exceeds 8 MB"
			return true
		}
		var reply struct {
			Path string `json:"path"`
		}
		if err := bridge.Command("paste_image", map[string]any{"data": base64.StdEncoding.EncodeToString(data)}, &reply); err != nil {
			a.notice = err.Error()
		} else {
			a.attachments = append(a.attachments, reply.Path)
		}
		return true
	}
	return false
}
