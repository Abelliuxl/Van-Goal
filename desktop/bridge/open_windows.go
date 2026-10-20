//go:build windows

package bridge

import "syscall"

func open(path string) (uintptr, error) {
	handle, err := syscall.LoadLibrary(path)
	return uintptr(handle), err
}
