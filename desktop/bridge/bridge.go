// Package bridge loads libvan_goal_desktop, the Rust core behind the app, and
// speaks its four-function C ABI: commands in, a JSON update out per poll.
//
// The library owns the connection, the session state machine and the message
// list (crates/desktop); this side only draws what it reports. Calls go
// through purego, so the app builds without cgo.
package bridge

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"sync"
	"unsafe"

	"github.com/ebitengine/purego"
)

var (
	loadOnce sync.Once
	loadErr  error

	// The strings the library returns stay *byte, never uintptr: a pointer
	// held as an integer and turned back into one is what go vet rejects.
	vgdCommand    func(string) *byte
	vgdPoll       func() *byte
	vgdWait       func(uint32) int32
	vgdFree       func(*byte)
	vgdSetDataDir func(string) int32
)

// LibraryName is the file the library is built as on this platform.
func LibraryName() string {
	switch runtime.GOOS {
	case "darwin":
		return "libvan_goal_desktop.dylib"
	case "windows":
		return "van_goal_desktop.dll"
	default:
		return "libvan_goal_desktop.so"
	}
}

// Load opens the library from the first of dirs that holds it. VAN_GOAL_LIB
// names the file outright, for development.
func Load(dirs ...string) error {
	loadOnce.Do(func() { loadErr = load(dirs) })
	return loadErr
}

func load(dirs []string) error {
	var candidates []string
	if path := os.Getenv("VAN_GOAL_LIB"); path != "" {
		candidates = append(candidates, path)
	}
	for _, dir := range dirs {
		candidates = append(candidates, filepath.Join(dir, LibraryName()))
	}
	var tried []string
	for _, path := range candidates {
		if _, err := os.Stat(path); err != nil {
			tried = append(tried, path)
			continue
		}
		handle, err := open(path)
		if err != nil {
			return fmt.Errorf("loading %s: %w", path, err)
		}
		purego.RegisterLibFunc(&vgdCommand, handle, "vgd_command")
		purego.RegisterLibFunc(&vgdPoll, handle, "vgd_poll")
		purego.RegisterLibFunc(&vgdWait, handle, "vgd_wait")
		purego.RegisterLibFunc(&vgdFree, handle, "vgd_free")
		purego.RegisterLibFunc(&vgdSetDataDir, handle, "vgd_set_data_dir")
		return nil
	}
	return fmt.Errorf("%s not found (looked in %v)", LibraryName(), tried)
}

// take copies a string the library handed out and releases it.
func take(p *byte) []byte {
	if p == nil {
		return nil
	}
	n := 0
	for *(*byte)(unsafe.Add(unsafe.Pointer(p), n)) != 0 {
		n++
	}
	out := make([]byte, n)
	copy(out, unsafe.Slice(p, n))
	vgdFree(p)
	return out
}

// SetDataDir points the app-data directory elsewhere; only tests use it, and
// it must come before any other call.
func SetDataDir(dir string) bool {
	return vgdSetDataDir(dir) == 1
}

// Command runs one command. Its reply's fields beside "ok" decode into out,
// when out is not nil.
func Command(cmd string, args map[string]any, out any) error {
	payload := map[string]any{"cmd": cmd}
	for k, v := range args {
		payload[k] = v
	}
	data, err := json.Marshal(payload)
	if err != nil {
		return err
	}
	reply := take(vgdCommand(string(data)))
	var status struct {
		OK    bool   `json:"ok"`
		Error string `json:"error"`
	}
	if err := json.Unmarshal(reply, &status); err != nil {
		return fmt.Errorf("unreadable reply to %s: %w", cmd, err)
	}
	if !status.OK {
		if status.Error == "" {
			status.Error = cmd + " failed"
		}
		return errors.New(status.Error)
	}
	if out != nil {
		return json.Unmarshal(reply, out)
	}
	return nil
}

// Wait blocks until the library has something new, or the timeout passes.
func Wait(timeoutMS uint32) bool {
	return vgdWait(timeoutMS) == 1
}

// Poll takes what changed since the last poll.
func Poll() (Update, error) {
	var update Update
	err := json.Unmarshal(take(vgdPoll()), &update)
	return update, err
}
