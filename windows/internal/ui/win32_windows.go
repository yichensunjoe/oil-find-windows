//go:build windows

package ui

import (
	"errors"
	"fmt"
	"runtime"
	"syscall"
	"unsafe"
)

var (
	user32   = syscall.NewLazyDLL("user32.dll")
	kernel32 = syscall.NewLazyDLL("kernel32.dll")
	shell32  = syscall.NewLazyDLL("shell32.dll")
	ole32    = syscall.NewLazyDLL("ole32.dll")
	comctl32 = syscall.NewLazyDLL("comctl32.dll")
	gdi32    = syscall.NewLazyDLL("gdi32.dll")

	createWindowEx       = user32.NewProc("CreateWindowExW")
	defWindowProc        = user32.NewProc("DefWindowProcW")
	registerClassEx      = user32.NewProc("RegisterClassExW")
	getMessage           = user32.NewProc("GetMessageW")
	translateMessage     = user32.NewProc("TranslateMessage")
	dispatchMessage      = user32.NewProc("DispatchMessageW")
	postMessage          = user32.NewProc("PostMessageW")
	postQuitMessage      = user32.NewProc("PostQuitMessage")
	sendMessage          = user32.NewProc("SendMessageW")
	showWindow           = user32.NewProc("ShowWindow")
	updateWindow         = user32.NewProc("UpdateWindow")
	destroyWindow        = user32.NewProc("DestroyWindow")
	moveWindow           = user32.NewProc("MoveWindow")
	setWindowPos         = user32.NewProc("SetWindowPos")
	getClientRect        = user32.NewProc("GetClientRect")
	getWindowText        = user32.NewProc("GetWindowTextW")
	getWindowTextLength  = user32.NewProc("GetWindowTextLengthW")
	setWindowText        = user32.NewProc("SetWindowTextW")
	enableWindow         = user32.NewProc("EnableWindow")
	setFocus             = user32.NewProc("SetFocus")
	getFocus             = user32.NewProc("GetFocus")
	getKeyState          = user32.NewProc("GetKeyState")
	setTimer             = user32.NewProc("SetTimer")
	killTimer            = user32.NewProc("KillTimer")
	messageBox           = user32.NewProc("MessageBoxW")
	loadCursor           = user32.NewProc("LoadCursorW")
	isDialogMessage      = user32.NewProc("IsDialogMessageW")
	openClipboard        = user32.NewProc("OpenClipboard")
	closeClipboard       = user32.NewProc("CloseClipboard")
	emptyClipboard       = user32.NewProc("EmptyClipboard")
	setClipboardData     = user32.NewProc("SetClipboardData")
	getModuleHandle      = kernel32.NewProc("GetModuleHandleW")
	getDriveType         = kernel32.NewProc("GetDriveTypeW")
	getVolumePathName    = kernel32.NewProc("GetVolumePathNameW")
	globalAlloc          = kernel32.NewProc("GlobalAlloc")
	globalLock           = kernel32.NewProc("GlobalLock")
	globalUnlock         = kernel32.NewProc("GlobalUnlock")
	globalFree           = kernel32.NewProc("GlobalFree")
	initCommonControlsEx = comctl32.NewProc("InitCommonControlsEx")
	browseForFolder      = shell32.NewProc("SHBrowseForFolderW")
	getPathFromIDList    = shell32.NewProc("SHGetPathFromIDListEx")
	shellExecute         = shell32.NewProc("ShellExecuteW")
	parseDisplayName     = shell32.NewProc("SHParseDisplayName")
	openFolderSelect     = shell32.NewProc("SHOpenFolderAndSelectItems")
	coInitialize         = ole32.NewProc("CoInitializeEx")
	coUninitialize       = ole32.NewProc("CoUninitialize")
	coTaskMemFree        = ole32.NewProc("CoTaskMemFree")
	createFont           = gdi32.NewProc("CreateFontW")
	deleteObject         = gdi32.NewProc("DeleteObject")
	getSysColorBrush     = user32.NewProc("GetSysColorBrush")
)

const (
	wmCreate       = 0x0001
	wmDestroy      = 0x0002
	wmSize         = 0x0005
	wmClose        = 0x0010
	wmSetFont      = 0x0030
	wmNotify       = 0x004e
	wmCommand      = 0x0111
	wmTimer        = 0x0113
	wmKeyDown      = 0x0100
	wmDpiChanged   = 0x02e0
	wmAppEvent     = 0x8001
	wsChild        = 0x40000000
	wsVisible      = 0x10000000
	wsTabStop      = 0x00010000
	wsBorder       = 0x00800000
	wsVScroll      = 0x00200000
	wsClipChildren = 0x02000000
	wsOverlapped   = 0x00cf0000
	wsExClientEdge = 0x00000200
	wsExControl    = 0x00010000
	lvmFirst       = 0x1000
	lvmSetExtStyle = lvmFirst + 54
	lvmInsertCol   = lvmFirst + 97
	lvmInsertItem  = lvmFirst + 77
	lvmSetItem     = lvmFirst + 76
	lvmDeleteAll   = lvmFirst + 9
	lvmGetNextItem = lvmFirst + 12
	lvmSetColWidth = lvmFirst + 30
	cbAddString    = 0x0143
	cbSetCurSel    = 0x014e
	cbGetCurSel    = 0x0147
	lbAddString    = 0x0180
	lbResetContent = 0x0184
	lbGetCurSel    = 0x0188
	enChange       = 0x0300
	cbnSelChange   = 1
	nmDoubleClick  = -3
	lvnItemChanged = -101
	searchTimer    = 1
	syncTimer      = 2
)

type point struct{ X, Y int32 }
type rect struct{ Left, Top, Right, Bottom int32 }
type winMessage struct {
	Hwnd    uintptr
	Message uint32
	WParam  uintptr
	LParam  uintptr
	Time    uint32
	Point   point
	Private uint32
}
type wndClass struct {
	Size       uint32
	Style      uint32
	WndProc    uintptr
	ClsExtra   int32
	WndExtra   int32
	Instance   uintptr
	Icon       uintptr
	Cursor     uintptr
	Background uintptr
	MenuName   *uint16
	ClassName  *uint16
	SmallIcon  uintptr
}
type commonControls struct{ Size, Classes uint32 }
type notifyHeader struct {
	Hwnd uintptr
	ID   uintptr
	Code uint32
}
type listColumn struct {
	Mask         uint32
	Format       int32
	Width        int32
	Text         *uint16
	TextMax      int32
	SubItem      int32
	Image        int32
	Order        int32
	MinWidth     int32
	DefaultWidth int32
	IdealWidth   int32
}
type listItem struct {
	Mask       uint32
	Item       int32
	SubItem    int32
	State      uint32
	StateMask  uint32
	Text       *uint16
	TextMax    int32
	Image      int32
	Param      uintptr
	Indent     int32
	GroupID    int32
	Columns    uint32
	ColumnList *uint32
	ColFormats *int32
	Group      int32
}
type browseInfo struct {
	Owner       uintptr
	Root        uintptr
	DisplayName *uint16
	Title       *uint16
	Flags       uint32
	Callback    uintptr
	Param       uintptr
	Image       int32
}

func wide(s string) *uint16 {
	p, _ := syscall.UTF16PtrFromString(s)
	return p
}

func nativeError(operation string, err error) error {
	if err == nil || err == syscall.Errno(0) {
		return errors.New(operation + "失败")
	}
	return fmt.Errorf("%s失败：%w", operation, err)
}

func createControl(owner, instance uintptr, class, label string, style, exStyle uint32, id int) (uintptr, error) {
	// Hold both UTF-16 buffers in locals: passing only uintptr(unsafe.Pointer(x))
	// lets the GC free them before the syscall reads them.
	classPtr, labelPtr := wide(class), wide(label)
	h, _, err := createWindowEx.Call(uintptr(exStyle), uintptr(unsafe.Pointer(classPtr)), uintptr(unsafe.Pointer(labelPtr)), uintptr(style|wsChild|wsVisible), 0, 0, 100, 28, owner, uintptr(id), instance, 0)
	runtime.KeepAlive(classPtr)
	runtime.KeepAlive(labelPtr)
	if h == 0 {
		return 0, nativeError("创建界面控件", err)
	}
	return h, nil
}

func text(h uintptr) string {
	n, _, _ := getWindowTextLength.Call(h)
	if n > 32768 {
		n = 32768
	}
	buf := make([]uint16, n+1)
	getWindowText.Call(h, uintptr(unsafe.Pointer(&buf[0])), uintptr(len(buf)))
	return syscall.UTF16ToString(buf)
}

func putText(h uintptr, s string) {
	ptr := wide(s)
	setWindowText.Call(h, uintptr(unsafe.Pointer(ptr)))
	runtime.KeepAlive(ptr)
}

func enabled(h uintptr, yes bool) {
	v := uintptr(0)
	if yes {
		v = 1
	}
	enableWindow.Call(h, v)
}

func warn(owner uintptr, s string) {
	textPtr, titlePtr := wide(s), wide("Oil Find for Windows")
	messageBox.Call(owner, uintptr(unsafe.Pointer(textPtr)), uintptr(unsafe.Pointer(titlePtr)), 0x30)
	runtime.KeepAlive(textPtr)
	runtime.KeepAlive(titlePtr)
}

func chooseFolder(owner uintptr) (string, error) {
	buf := make([]uint16, 260)
	info := browseInfo{Owner: owner, DisplayName: &buf[0], Title: wide("选择本机目录（不支持网络共享或网络映射盘）"), Flags: 0x0001 | 0x0040}
	pidl, _, _ := browseForFolder.Call(uintptr(unsafe.Pointer(&info)))
	runtime.KeepAlive(info)
	runtime.KeepAlive(buf)
	if pidl == 0 {
		return "", nil
	}
	defer coTaskMemFree.Call(pidl)
	buf = make([]uint16, 32768)
	ok, _, err := getPathFromIDList.Call(pidl, uintptr(unsafe.Pointer(&buf[0])), uintptr(len(buf)), 0)
	if ok == 0 {
		return "", nativeError("读取所选目录", err)
	}
	return syscall.UTF16ToString(buf), nil
}

func copyToClipboard(owner uintptr, value string) error {
	buf, err := syscall.UTF16FromString(value)
	if err != nil {
		return err
	}
	h, _, err := globalAlloc.Call(0x0002, uintptr(len(buf)*2))
	if h == 0 {
		return nativeError("分配剪贴板内容", err)
	}
	transferred := false
	defer func() {
		if !transferred {
			globalFree.Call(h)
		}
	}()
	p, _, err := globalLock.Call(h)
	if p == 0 {
		return nativeError("访问剪贴板内容", err)
	}
	copy(unsafe.Slice((*uint16)(unsafe.Pointer(p)), len(buf)), buf)
	globalUnlock.Call(h)
	ok, _, err := openClipboard.Call(owner)
	if ok == 0 {
		return nativeError("打开剪贴板", err)
	}
	defer closeClipboard.Call()
	ok, _, err = emptyClipboard.Call()
	if ok == 0 {
		return nativeError("更新剪贴板", err)
	}
	ok, _, err = setClipboardData.Call(13, h)
	if ok == 0 {
		return nativeError("复制路径", err)
	}
	transferred = true
	return nil
}

func openPath(owner uintptr, path string) error {
	verb, target := wide("open"), wide(path)
	r, _, _ := shellExecute.Call(owner, uintptr(unsafe.Pointer(verb)), uintptr(unsafe.Pointer(target)), 0, 0, 1)
	runtime.KeepAlive(verb)
	runtime.KeepAlive(target)
	if r <= 32 {
		return fmt.Errorf("无法打开所选项目（系统代码 %d）。请检查默认打开程序与访问权限", r)
	}
	return nil
}

func revealPath(path string) error {
	var pidl uintptr
	pathPtr := wide(path)
	hr, _, _ := parseDisplayName.Call(uintptr(unsafe.Pointer(pathPtr)), 0, uintptr(unsafe.Pointer(&pidl)), 0, 0)
	runtime.KeepAlive(pathPtr)
	if int32(hr) < 0 || pidl == 0 {
		return fmt.Errorf("无法定位所选项目（系统代码 0x%08x）", uint32(hr))
	}
	defer coTaskMemFree.Call(pidl)
	hr, _, _ = openFolderSelect.Call(pidl, 0, 0, 0)
	if int32(hr) < 0 {
		return fmt.Errorf("无法打开资源管理器（系统代码 0x%08x）", uint32(hr))
	}
	return nil
}
