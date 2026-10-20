package main

import "runtime"

// isMac reports whether the window runs on macOS: the platform whose
// shortcuts use Command and whose window controls sit on the left.
func isMac() bool { return runtime.GOOS == "darwin" }

// shortcut names a shortcut the way this platform writes it in a hint: ⌘N on
// macOS, Ctrl+N elsewhere.
func shortcut(key string) string {
	if isMac() {
		return "⌘" + key
	}
	return "Ctrl+" + key
}

// thisComputer names the machine in a confirmation about local data.
func thisComputer() string {
	if isMac() {
		return "this Mac"
	}
	return "this PC"
}

// sendHint is the line under the composer: how to send, and how to keep a
// newline. Mac keyboards label the key with a return arrow; Windows ones
// spell it out.
func sendHint() string {
	if isMac() {
		return "↩ to send · ⇧↩ for a new line"
	}
	return "Enter to send · Shift+Enter for a new line"
}

func queueHint() string {
	if isMac() {
		return "↩ to queue a follow-up · esc to stop"
	}
	return "Enter to queue a follow-up · Esc to stop"
}

// workspacePlaceholder shows the shape of an absolute path on this platform;
// the field stores exactly what is typed.
func workspacePlaceholder() string {
	if isMac() {
		return "~/Projects/…"
	}
	return `C:\Projects\…`
}
