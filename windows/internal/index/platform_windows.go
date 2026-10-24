//go:build windows

package index

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"unsafe"
)

var kernel32 = syscall.NewLazyDLL("kernel32.dll")
var getDriveType = kernel32.NewProc("GetDriveTypeW")
var moveFileEx = kernel32.NewProc("MoveFileExW")

func validateLocalRoot(root string) error {
	volume := filepath.VolumeName(root)
	if len(volume) != 2 || volume[1] != ':' {
		return errors.New("仅支持本机磁盘，不支持 UNC、设备或网络目录")
	}
	drive, err := syscall.UTF16PtrFromString(volume + `\`)
	if err != nil {
		return err
	}
	kind, _, _ := getDriveType.Call(uintptr(unsafe.Pointer(drive)))
	if kind != 2 && kind != 3 {
		return errors.New("仅支持本机固定磁盘或已连接的移动磁盘，不支持网络映射盘")
	}
	return nil
}
func isLinkOrReparse(path string, d os.DirEntry) bool {
	if d.Type()&os.ModeSymlink != 0 {
		return true
	}
	// os.Lstat applies the \\?\ long-path conversion that GetFileAttributesW
	// does not, so entries deeper than MAX_PATH are still classified correctly
	// instead of being reported as unreadable (and therefore skipped).
	info, err := os.Lstat(path)
	if err != nil {
		return true
	}
	data, ok := info.Sys().(*syscall.Win32FileAttributeData)
	return !ok || data.FileAttributes&0x400 != 0
}
func platformPathEqual(a, b string) bool { return strings.EqualFold(a, b) }
func platformPathPrefix(a, b string) bool {
	return strings.HasPrefix(strings.ToLower(a), strings.ToLower(b))
}
func replaceCache(source, dest string) error {
	a, err := syscall.UTF16PtrFromString(source)
	if err != nil {
		return err
	}
	b, err := syscall.UTF16PtrFromString(dest)
	if err != nil {
		return err
	}
	// MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH, same-directory replacement.
	ok, _, callErr := moveFileEx.Call(uintptr(unsafe.Pointer(a)), uintptr(unsafe.Pointer(b)), 1|8)
	if ok == 0 {
		return callErr
	}
	return nil
}
