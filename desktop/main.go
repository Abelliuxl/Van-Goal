// Van-Goal is a client for coding agents. This is its desktop app, drawn by
// MyGo; the Rust core in crates/desktop speaks to the agents.
//
//	cargo build -p van-goal-desktop && go run .     # from desktop/, in development
//	Scripts/package_app.sh                          # Build/VanGoal.app
package main

import (
	"log"
	"os"
	"path/filepath"
	"time"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"

	"github.com/Abelliuxl/Van-Goal/desktop/bridge"
)

// version is stamped by the packaging scripts from the Cargo workspace
// version; `go run` keeps the default.
var version = "0.1.0"

func main() {
	if err := bridge.Load(libraryDirs()...); err != nil {
		log.Fatalf("van-goal: %v", err)
	}
	// Development only: run on settings and a cache of its own, leaving the
	// real ones alone.
	if dir := os.Getenv("VAN_GOAL_DATA_DIR"); dir != "" {
		bridge.SetDataDir(dir)
	}
	a := newApp()
	var first bridge.Update
	if err := bridge.Command("start", nil, &first); err != nil {
		log.Fatalf("van-goal: %v", err)
	}
	a.apply(first)

	mygo.App.SetName("Van-Goal")
	mygo.App.WhenReady(func() {
		mygo.App.SetMenu(a.menu())
		a.openWindow()
		go a.pump()
	})
	// Reopening from the Dock brings the window back (macOS).
	mygo.App.OnActivate(func(visible bool) {
		if !visible && (a.win == nil || a.win.IsDestroyed()) {
			a.openWindow()
		}
	})
	mygo.App.OnBeforeQuit(func(*mygo.QuitEvent) {
		_ = bridge.Command("shutdown", nil, nil)
	})
	if err := mygo.App.Run(); err != nil {
		log.Fatal(err)
	}
}

func (a *app) openWindow() {
	opts := mygo.WindowOptions{
		Title:         "Van-Goal",
		Width:         1180,
		Height:        780,
		MinWidth:      640,
		MinHeight:     480,
		TitleBarStyle: mygo.TitleBarHidden,
		// On Windows the caption buttons fill this height; matching the
		// header's keeps them on the same line as its last button instead of
		// sitting in the top half of it. macOS ignores it.
		TitleBarHeight: 46,
		Content:        ui.View(a.view),
	}
	if w := a.settings.Window; w != nil {
		opts.X, opts.Y = int(w.X), int(w.Y)
		opts.Width, opts.Height = int(w.Width), int(w.Height)
		opts.Maximized = w.Maximized
	}
	win := mygo.NewWindow(opts)
	a.win = win
	a.focusDraft = true
	// Windows takes the taskbar icon from the executable's resources when it
	// has them; this also covers a development run, where the icon is the PNG
	// beside the workspace's assets.
	if png := windowIcon(); png != nil {
		_ = win.SetIcon(png)
	}

	// The window's place is remembered once it stops moving.
	var timer *time.Timer
	save := func() {
		if timer != nil {
			timer.Stop()
		}
		timer = time.AfterFunc(400*time.Millisecond, func() {
			win.Update(func() {
				if win.IsDestroyed() || win.IsFullScreen() || win.IsMinimized() {
					return
				}
				b := win.Bounds()
				_ = a.setting(map[string]any{"window": map[string]any{
					"x": b.X, "y": b.Y, "width": b.Width, "height": b.Height, "maximized": win.IsMaximized(),
				}})
			})
		})
	}
	win.OnResize(save)
	win.OnMove(save)
	win.OnMaximize(save)
	win.OnUnmaximize(save)
}

func (a *app) menu() *mygo.Menu {
	click := func(fn func()) func(*mygo.MenuItem, *mygo.Window) {
		return func(*mygo.MenuItem, *mygo.Window) {
			if a.win != nil {
				a.win.Update(fn)
			}
		}
	}
	file := &mygo.MenuItem{Label: "File", Submenu: []*mygo.MenuItem{
		{Label: "New Chat", Accelerator: "CmdOrCtrl+N", Click: click(a.newChat)},
		{Label: "Attach Files…", Accelerator: "CmdOrCtrl+O", Click: click(a.pickFiles)},
	}}
	view := &mygo.MenuItem{Label: "View", Submenu: []*mygo.MenuItem{
		{Label: "Toggle Sidebar", Accelerator: "CmdOrCtrl+B", Click: click(a.toggleSidebar)},
		{Label: "Refresh", Accelerator: "CmdOrCtrl+R", Click: click(func() { a.do("refresh", nil) })},
		mygo.Separator(),
		{Label: "Zoom In", Accelerator: "CmdOrCtrl+Plus", Click: click(a.zoomIn)},
		{Label: "Zoom Out", Accelerator: "CmdOrCtrl+-", Click: click(a.zoomOut)},
		{Label: "Actual Size", Accelerator: "CmdOrCtrl+0", Click: click(a.actualSize)},
		mygo.Separator(),
		{Role: mygo.RoleToggleFullScreen},
	}}
	if isMac() {
		file.Submenu = append(file.Submenu, mygo.Separator(), &mygo.MenuItem{Role: mygo.RoleClose})
		return mygo.NewMenu([]*mygo.MenuItem{
			{Role: mygo.RoleAppMenu, Submenu: []*mygo.MenuItem{
				{Role: mygo.RoleAbout},
				mygo.Separator(),
				{Label: "Settings…", Accelerator: "CmdOrCtrl+,", Click: click(a.openSettings)},
				mygo.Separator(),
				{Role: mygo.RoleHide},
				{Role: mygo.RoleHideOthers},
				{Role: mygo.RoleUnhide},
				mygo.Separator(),
				{Role: mygo.RoleQuit},
			}},
			file,
			{Role: mygo.RoleEditMenu},
			view,
			{Role: mygo.RoleWindowMenu},
		})
	}
	// Windows has no application menu: a Settings item inside one would be
	// hidden with it, and Close would leave no way to quit the app. Settings
	// goes in File, About in Help, and Exit replaces Close.
	file.Submenu = append(file.Submenu,
		mygo.Separator(),
		&mygo.MenuItem{Label: "Settings…", Accelerator: "CmdOrCtrl+,", Click: click(a.openSettings)},
		mygo.Separator(),
		&mygo.MenuItem{Role: mygo.RoleQuit},
	)
	return mygo.NewMenu([]*mygo.MenuItem{
		file,
		{Role: mygo.RoleEditMenu},
		view,
		{Label: "Help", Submenu: []*mygo.MenuItem{
			{Label: "About Van-Goal", Click: click(a.showAbout)},
		}},
	})
}

// showAbout is the About dialog the Help menu opens on Windows, where the
// role that shows the system's own panel is macOS-only.
func (a *app) showAbout() {
	go func() {
		_, _ = mygo.Dialog.Message(mygo.MessageOptions{
			Parent:  a.win,
			Type:    mygo.MessageInfo,
			Title:   "About Van-Goal",
			Message: "Van-Goal " + version,
			Detail:  "A desktop client for coding agents.\nDrawn by MyGo over the Rust core.",
			Buttons: []string{"OK"},
		})
	}()
}

// windowIcon is the app's PNG icon beside the executable, in its resources,
// or in the workspace while developing. A packaged Windows build also embeds
// it as the executable's icon; setting it here gives the window and its
// taskbar button the same one in a development run.
func windowIcon() []byte {
	var candidates []string
	if exe, err := os.Executable(); err == nil {
		dir := filepath.Dir(exe)
		candidates = append(candidates,
			filepath.Join(dir, "AppIcon.png"),
			filepath.Join(dir, "..", "Resources", "AppIcon.png"))
	}
	if wd, err := os.Getwd(); err == nil {
		candidates = append(candidates,
			filepath.Join(wd, "..", "assets", "AppIcon.png"),
			filepath.Join(wd, "assets", "AppIcon.png"))
	}
	for _, path := range candidates {
		if data, err := os.ReadFile(path); err == nil {
			return data
		}
	}
	return nil
}

// libraryDirs is where the Rust core may be: inside the app's resources once
// packaged, next to the executable, or in the workspace's target directory
// while developing.
func libraryDirs() []string {
	var dirs []string
	if dir, err := mygo.App.Path(mygo.PathResources); err == nil {
		dirs = append(dirs, dir)
	}
	if exe, err := os.Executable(); err == nil {
		dir := filepath.Dir(exe)
		dirs = append(dirs, dir, filepath.Join(dir, "..", "Resources"), filepath.Join(dir, "..", "Frameworks"))
	}
	if wd, err := os.Getwd(); err == nil {
		dirs = append(dirs, filepath.Join(wd, "..", "target", "release"), filepath.Join(wd, "..", "target", "debug"))
	}
	return dirs
}
