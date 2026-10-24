//go:build windows

package ui

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"syscall"
	"time"
	"unsafe"

	"oilfind-intranet/windows/internal/index"
)

const (
	idSearch = 100 + iota
	idAddRoot
	idRemoveRoot
	idStart
	idStop
	idRoots
	idTypes
	idResults
	idOpen
	idReveal
	idCopy
)

const resultLimit = 2000

var currentApp *application
var windowCallback = syscall.NewCallback(windowProc)

type configuration struct {
	Roots      []string `json:"roots"`
	CacheRoots []string `json:"cache_roots,omitempty"`
}

type uiEvent struct {
	kind       string
	generation uint64
	idx        *index.Index
	entries    []index.Entry
	total      int
	progress   index.Progress
	err        error
	saveErr    error
	updated    time.Time
}

type searchRequest struct {
	generation  uint64
	idx         *index.Index
	query, kind string
}

type application struct {
	hwnd, instance, font                         uintptr
	search, rootsList, types, results, status    uintptr
	add, remove, start, stop, open, reveal, copy uintptr
	labels                                       []uintptr
	dpi                                          int
	ctx                                          context.Context
	cancel                                       context.CancelFunc
	events                                       chan uiEvent
	searchRequests                               chan searchRequest
	scanGate                                     chan struct{}
	storageNotice                                string
	config                                       configuration
	configPath, cachePath                        string
	idx                                          *index.Index
	rows                                         []index.Entry
	scanCancel                                   context.CancelFunc
	scanGeneration, searchGeneration             uint64
	scanning, autoSync, closing                  bool
	lastUpdate                                   time.Time
	lastNotice                                   string
	startupNotice                                string
	initErr                                      error
}

func Run() (runErr error) {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	defer func() {
		if runErr != nil {
			warn(0, runErr.Error())
		}
	}()
	setDPIAwareness()
	hr, _, _ := coInitialize.Call(0, 2)
	if int32(hr) < 0 {
		return fmt.Errorf("初始化目录选择器失败（系统代码 0x%08x）", uint32(hr))
	}
	defer coUninitialize.Call()
	controls := commonControls{Size: uint32(unsafe.Sizeof(commonControls{})), Classes: 1}
	ok, _, err := initCommonControlsEx.Call(uintptr(unsafe.Pointer(&controls)))
	if ok == 0 {
		return nativeError("初始化原生列表控件", err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	a := &application{ctx: ctx, cancel: cancel, events: make(chan uiEvent, 128), searchRequests: make(chan searchRequest, 1), scanGate: make(chan struct{}, 1), dpi: 96}
	if p := user32.NewProc("GetDpiForSystem"); p.Find() == nil {
		dpi, _, _ := p.Call()
		if dpi >= 96 && dpi <= 480 {
			a.dpi = int(dpi)
		}
	}
	a.loadConfiguration()
	instance, _, err := getModuleHandle.Call(0)
	if instance == 0 {
		return nativeError("加载应用程序模块", err)
	}
	a.instance = instance
	className := wide("OilFindIntranetNativeWindow")
	cursor, _, _ := loadCursor.Call(0, 32512)
	brush, _, _ := getSysColorBrush.Call(5)
	class := wndClass{Size: uint32(unsafe.Sizeof(wndClass{})), Style: 3, WndProc: windowCallback, Instance: instance, Cursor: cursor, Background: brush, ClassName: className}
	atom, _, err := registerClassEx.Call(uintptr(unsafe.Pointer(&class)))
	if atom == 0 {
		return nativeError("注册应用窗口", err)
	}
	currentApp = a
	defer func() { currentApp = nil }()
	width, height := a.px(1120), a.px(760)
	metrics := user32.NewProc("GetSystemMetrics")
	screenWidth, _, _ := metrics.Call(0)
	screenHeight, _, _ := metrics.Call(1)
	if available := int(screenWidth) - a.px(32); available > 0 && width > available {
		width = available
	}
	if available := int(screenHeight) - a.px(48); available > 0 && height > available {
		height = available
	}
	hwnd, _, err := createWindowEx.Call(wsExControl, uintptr(unsafe.Pointer(className)), uintptr(unsafe.Pointer(wide("Oil Find 内网版 · Windows 预览版"))), wsOverlapped|wsClipChildren, 0x80000000, 0x80000000, uintptr(width), uintptr(height), 0, 0, instance, 0)
	if hwnd == 0 {
		if a.initErr != nil {
			return a.initErr
		}
		return nativeError("创建应用窗口", err)
	}
	defer func() {
		if a.font != 0 {
			deleteObject.Call(a.font)
		}
	}()
	showWindow.Call(hwnd, 5)
	updateWindow.Call(hwnd)
	a.startSearchWorker()
	a.loadCache()
	var msg winMessage
	for {
		r, _, err := getMessage.Call(uintptr(unsafe.Pointer(&msg)), 0, 0, 0)
		if int32(r) == -1 {
			destroyWindow.Call(hwnd)
			return nativeError("读取窗口消息", err)
		}
		if r == 0 {
			break
		}
		if a.keyboard(&msg) {
			continue
		}
		handled, _, _ := isDialogMessage.Call(hwnd, uintptr(unsafe.Pointer(&msg)))
		if handled == 0 {
			translateMessage.Call(uintptr(unsafe.Pointer(&msg)))
			dispatchMessage.Call(uintptr(unsafe.Pointer(&msg)))
		}
	}
	return nil
}

func windowProc(hwnd uintptr, msg uint32, wParam, lParam uintptr) uintptr {
	a := currentApp
	if a == nil {
		r, _, _ := defWindowProc.Call(hwnd, uintptr(msg), wParam, lParam)
		return r
	}
	switch msg {
	case wmCreate:
		a.hwnd = hwnd
		if err := a.createUI(); err != nil {
			a.initErr = err
			return ^uintptr(0)
		}
		return 0
	case wmSize:
		if a.results != 0 {
			a.layout()
		}
		return 0
	case wmDpiChanged:
		if lParam != 0 && a.results != 0 && !a.closing {
			suggested := *(*rect)(unsafe.Pointer(lParam))
			if err := a.changeDPI(int(wParam&0xffff), suggested); err != nil {
				a.lastNotice = "调整显示缩放失败，保留现有界面：" + err.Error()
				a.renderStatus()
			}
		}
		return 0
	case 0x0024:
		if lParam != 0 {
			limits := (*struct{ Reserved, MaxSize, MaxPosition, MinTrack, MaxTrack point })(unsafe.Pointer(lParam))
			limits.MinTrack = point{X: int32(a.px(860)), Y: int32(a.px(620))}
		}
		return 0
	case wmCommand:
		a.command(int(wParam&0xffff), int((wParam>>16)&0xffff))
		return 0
	case wmNotify:
		if lParam != 0 {
			n := (*notifyHeader)(unsafe.Pointer(lParam))
			if n.Hwnd == a.results {
				if int32(n.Code) == nmDoubleClick {
					activation := (*struct {
						Header        notifyHeader
						Item, SubItem int32
					})(unsafe.Pointer(lParam))
					if activation.Item >= 0 && int(activation.Item) < len(a.rows) {
						a.fileAction(idOpen)
					}
				} else if int32(n.Code) == lvnItemChanged {
					a.selectionButtons()
				}
			}
		}
		return 0
	case wmTimer:
		if wParam == searchTimer {
			killTimer.Call(hwnd, searchTimer)
			a.searchNow()
		} else if wParam == syncTimer && a.autoSync && !a.scanning {
			a.beginScan()
		}
		return 0
	case wmAppEvent:
		for {
			select {
			case event := <-a.events:
				a.handleEvent(event)
			default:
				return 0
			}
		}
	case wmClose:
		a.shutdown()
		destroyWindow.Call(hwnd)
		return 0
	case wmDestroy:
		a.shutdown()
		postQuitMessage.Call(0)
		return 0
	}
	r, _, _ := defWindowProc.Call(hwnd, uintptr(msg), wParam, lParam)
	return r
}

func (a *application) createUI() error {
	if p := user32.NewProc("GetDpiForWindow"); p.Find() == nil {
		dpi, _, _ := p.Call(a.hwnd)
		if dpi >= 96 && dpi <= 480 {
			a.dpi = int(dpi)
		}
	}
	font, _, err := createFont.Call(uintptr(int64(-a.px(17))), 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0, uintptr(unsafe.Pointer(wide("Microsoft YaHei UI"))))
	if font == 0 {
		return nativeError("创建界面字体", err)
	}
	a.font = font
	makeControl := func(class, label string, style, ex uint32, id int, target *uintptr) error {
		h, err := createControl(a.hwnd, a.instance, class, label, style, ex, id)
		if err != nil {
			return err
		}
		*target = h
		sendMessage.Call(h, wmSetFont, font, 1)
		return nil
	}
	for _, label := range []string{"搜索本机文件", "索引范围（仅本机目录；删除范围不会删除文件）", "文件类型", "索引只记录文件名与路径等元数据，不上传文件内容。"} {
		var h uintptr
		if err := makeControl("STATIC", label, 0x0080, 0, 0, &h); err != nil {
			return err
		}
		a.labels = append(a.labels, h)
	}
	if err := makeControl("EDIT", "", wsTabStop|0x0080, wsExClientEdge, idSearch, &a.search); err != nil {
		return err
	}
	sendMessage.Call(a.search, 0x00c5, 4096, 0)
	sendMessage.Call(a.search, 0x1501, 0, uintptr(unsafe.Pointer(wide("中文 / 拼音 / 空格 AND / !排除 / OR 或 | / ext:pdf / size:>1mb"))))
	for _, item := range []struct {
		label  string
		id     int
		target *uintptr
	}{
		{"添加目录…", idAddRoot, &a.add}, {"删除范围", idRemoveRoot, &a.remove},
		{"开始索引", idStart, &a.start}, {"停止 / 暂停同步", idStop, &a.stop},
		{"打开所选", idOpen, &a.open}, {"资源管理器定位", idReveal, &a.reveal}, {"复制路径", idCopy, &a.copy},
	} {
		if err := makeControl("BUTTON", item.label, wsTabStop, 0, item.id, item.target); err != nil {
			return err
		}
	}
	if err := makeControl("LISTBOX", "", wsTabStop|wsVScroll|0x0001|0x0100, wsExClientEdge, idRoots, &a.rootsList); err != nil {
		return err
	}
	if err := makeControl("COMBOBOX", "", wsTabStop|wsVScroll|0x0003, 0, idTypes, &a.types); err != nil {
		return err
	}
	for _, label := range []string{"全部类型", "仅文件", "仅文件夹", "文档", "图片", "音频", "视频", "压缩包"} {
		sendMessage.Call(a.types, cbAddString, 0, uintptr(unsafe.Pointer(wide(label))))
	}
	sendMessage.Call(a.types, cbSetCurSel, 0, 0)
	if err := makeControl("SysListView32", "", wsTabStop|wsBorder|0x0001|0x0004|0x0008, wsExClientEdge, idResults, &a.results); err != nil {
		return err
	}
	sendMessage.Call(a.results, lvmSetExtStyle, 0, 0x0020|0x0001|0x00010000)
	for n, label := range []string{"文件名", "完整路径", "大小", "修改日期"} {
		column := listColumn{Mask: 0x0001 | 0x0002 | 0x0004 | 0x0008, Width: int32(a.px(180)), Text: wide(label), SubItem: int32(n)}
		r, _, err := sendMessage.Call(a.results, lvmInsertCol, uintptr(n), uintptr(unsafe.Pointer(&column)))
		if int32(r) == -1 {
			return nativeError("创建结果列表列", err)
		}
	}
	if err := makeControl("STATIC", "", 0x0080, 0, 0, &a.status); err != nil {
		return err
	}
	a.refreshRoots()
	a.selectionButtons()
	enabled(a.stop, false)
	a.lastNotice = "尚未启动索引。请先添加本机目录，再明确点击“开始索引”。"
	if a.startupNotice != "" {
		a.lastNotice = a.startupNotice
	}
	a.renderStatus()
	a.layout()
	setFocus.Call(a.search)
	return nil
}

// dpiAwarenessPerMonitorV2 是 DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2（-4）的句柄值，
// 与 assets/app.manifest 中的 PerMonitorV2 声明保持一致。
const dpiAwarenessPerMonitorV2 = ^uintptr(3)

func setDPIAwareness() {
	if p := user32.NewProc("SetProcessDpiAwarenessContext"); p.Find() == nil {
		if ok, _, _ := p.Call(dpiAwarenessPerMonitorV2); ok != 0 {
			return
		}
	}
	// 清单已声明或系统不支持时，回退到旧的系统级 DPI 感知（失败不影响启动）。
	user32.NewProc("SetProcessDPIAware").Call()
}

func (a *application) changeDPI(dpi int, suggested rect) error {
	if dpi < 96 || dpi > 768 || suggested.Right <= suggested.Left || suggested.Bottom <= suggested.Top {
		return errors.New("显示缩放参数无效")
	}
	font, _, err := createFont.Call(uintptr(int64(-(17 * dpi / 96))), 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0, uintptr(unsafe.Pointer(wide("Microsoft YaHei UI"))))
	if font == 0 {
		return nativeError("创建缩放字体", err)
	}
	old := a.font
	a.font, a.dpi = font, dpi
	controls := append([]uintptr(nil), a.labels...)
	controls = append(controls, a.search, a.rootsList, a.types, a.results, a.status, a.add, a.remove, a.start, a.stop, a.open, a.reveal, a.copy)
	for _, handle := range controls {
		if handle != 0 {
			sendMessage.Call(handle, wmSetFont, font, 1)
		}
	}
	// 按系统建议矩形重建窗口；失败时保留原几何，后续 WM_SIZE 仍会重排。
	setWindowPos.Call(a.hwnd, 0, uintptr(suggested.Left), uintptr(suggested.Top), uintptr(suggested.Right-suggested.Left), uintptr(suggested.Bottom-suggested.Top), 0x0004|0x0010)
	a.layout()
	if old != 0 {
		deleteObject.Call(old)
	}
	return nil
}

func (a *application) px(value int) int { return value * a.dpi / 96 }

func (a *application) layout() {
	var area rect
	getClientRect.Call(a.hwnd, uintptr(unsafe.Pointer(&area)))
	w, h := int(area.Right), int(area.Bottom)
	p, gap, row := a.px(18), a.px(10), a.px(32)
	move := func(handle uintptr, x, y, width, height int) {
		if width < 1 {
			width = 1
		}
		if height < 1 {
			height = 1
		}
		moveWindow.Call(handle, uintptr(x), uintptr(y), uintptr(width), uintptr(height), 1)
	}
	move(a.labels[0], p, p, w-2*p, a.px(25))
	y := p + a.px(29)
	move(a.search, p, y, w-2*p, row)
	y += row + gap
	move(a.labels[1], p, y, w-2*p, a.px(25))
	y += a.px(29)
	buttonsW := a.px(145)
	move(a.rootsList, p, y, w-3*p-buttonsW, a.px(100))
	move(a.add, w-p-buttonsW, y, buttonsW, row)
	move(a.remove, w-p-buttonsW, y+row+gap, buttonsW, row)
	y += a.px(100) + gap
	move(a.start, p, y, a.px(125), row)
	move(a.stop, p+a.px(135), y, a.px(165), row)
	move(a.labels[2], w-p-a.px(280), y+a.px(4), a.px(75), row)
	move(a.types, w-p-a.px(195), y, a.px(195), a.px(240))
	y += row + gap
	bottom := h - a.px(112)
	move(a.results, p, y, w-2*p, bottom-y)
	move(a.open, p, bottom+gap, a.px(115), row)
	move(a.reveal, p+a.px(125), bottom+gap, a.px(165), row)
	move(a.copy, p+a.px(300), bottom+gap, a.px(115), row)
	move(a.status, p, h-a.px(63), w-2*p, a.px(29))
	move(a.labels[3], p, h-a.px(30), w-2*p, a.px(24))
	for n, width := range []int{a.px(220), w - 2*p - a.px(505), a.px(100), a.px(165)} {
		if width < a.px(100) {
			width = a.px(100)
		}
		sendMessage.Call(a.results, lvmSetColWidth, uintptr(n), uintptr(width))
	}
}

func (a *application) keyboard(msg *winMessage) bool {
	if msg.Message != wmKeyDown {
		return false
	}
	control, _, _ := getKeyState.Call(0x11)
	if msg.WParam == 'L' && int16(control) < 0 {
		setFocus.Call(a.search)
		sendMessage.Call(a.search, 0x00b1, 0, ^uintptr(0))
		return true
	}
	if msg.WParam == 0x1b {
		putText(a.search, "")
		killTimer.Call(a.hwnd, searchTimer)
		a.searchGeneration++
		a.setRows(nil)
		a.lastNotice = "已取消搜索；索引与文件未改动。"
		a.renderStatus()
		return true
	}
	if msg.WParam == 0x0d {
		focus, _, _ := getFocus.Call()
		if focus == a.results {
			a.fileAction(idOpen)
			return true
		}
		if focus == a.search {
			killTimer.Call(a.hwnd, searchTimer)
			a.searchNow()
			setFocus.Call(a.results)
			return true
		}
	}
	return false
}

func (a *application) command(id, notification int) {
	if a.closing {
		return
	}
	switch id {
	case idSearch:
		if notification == enChange && a.search != 0 {
			a.searchGeneration++
			setTimer.Call(a.hwnd, searchTimer, 250, 0)
		}
	case idTypes:
		if notification == cbnSelChange {
			a.searchGeneration++
			a.searchNow()
		}
	case idAddRoot:
		if notification == 0 {
			a.addRoot()
		}
	case idRemoveRoot:
		if notification == 0 {
			a.removeRoot()
		}
	case idStart:
		if notification == 0 {
			if len(a.config.Roots) == 0 {
				warn(a.hwnd, "请先添加至少一个本机目录；应用不会自动选择或扫描个人目录。")
				return
			}
			a.autoSync = true
			setTimer.Call(a.hwnd, syncTimer, 30000, 0)
			a.beginScan()
		}
	case idStop:
		if notification == 0 {
			a.pauseScan()
			a.lastNotice = "已停止扫描并暂停 30 秒同步；已有索引仍可搜索。"
			a.renderStatus()
		}
	case idRoots:
		if notification == 1 {
			n, _, _ := sendMessage.Call(a.rootsList, lbGetCurSel, 0, 0)
			enabled(a.remove, int32(n) >= 0)
		}
	case idOpen, idReveal, idCopy:
		if notification == 0 {
			a.fileAction(id)
		}
	}
}

func (a *application) addRoot() {
	path, err := chooseFolder(a.hwnd)
	if err != nil {
		warn(a.hwnd, err.Error())
		return
	}
	if path == "" {
		return
	}
	path, err = localDirectory(path)
	if err != nil {
		warn(a.hwnd, err.Error())
		return
	}
	for _, root := range a.config.Roots {
		if withinRoot(root, path) {
			warn(a.hwnd, "该目录已包含在现有索引范围内。")
			return
		}
	}
	if len(a.config.Roots) >= 64 {
		warn(a.hwnd, "最多可配置 64 个索引目录。")
		return
	}
	a.invalidateScope()
	var roots []string
	for _, root := range a.config.Roots {
		if !withinRoot(path, root) {
			roots = append(roots, root)
		}
	}
	a.config.Roots = append(roots, path)
	a.scopeChanged("已添加本机目录。请点击“开始索引”；尚未自动扫描。")
}

func (a *application) removeRoot() {
	n, _, _ := sendMessage.Call(a.rootsList, lbGetCurSel, 0, 0)
	position := int(int32(n))
	if position < 0 || position >= len(a.config.Roots) {
		return
	}
	a.invalidateScope()
	a.config.Roots = append(a.config.Roots[:position], a.config.Roots[position+1:]...)
	a.scopeChanged("已删除索引范围配置，未删除任何个人文件。重新索引前不显示旧范围结果。")
}

func (a *application) invalidateScope() {
	a.pauseScan()
	a.searchGeneration++
	a.idx = nil
	a.config.CacheRoots = nil
	a.lastUpdate = time.Time{}
	a.setRows(nil)
}

func (a *application) scopeChanged(notice string) {
	a.lastNotice = notice
	if err := a.saveConfiguration(); err != nil {
		a.lastNotice += " 配置未保存：" + err.Error()
	}
	a.refreshRoots()
	a.renderStatus()
}

func (a *application) refreshRoots() {
	sendMessage.Call(a.rootsList, lbResetContent, 0, 0)
	for _, root := range a.config.Roots {
		sendMessage.Call(a.rootsList, lbAddString, 0, uintptr(unsafe.Pointer(wide(root))))
	}
	enabled(a.start, len(a.config.Roots) > 0 && !a.scanning)
	enabled(a.remove, false)
}

func sendEvent(ctx context.Context, events chan<- uiEvent, hwnd uintptr, event uiEvent, optional bool) {
	if ctx.Err() != nil {
		return
	}
	if optional {
		select {
		case events <- event:
		default:
			return
		}
	} else {
		select {
		case events <- event:
		case <-ctx.Done():
			return
		}
	}
	postMessage.Call(hwnd, wmAppEvent, 0, 0)
}

func (a *application) beginScan() {
	if a.scanning || len(a.config.Roots) == 0 {
		return
	}
	a.scanGeneration++
	generation := a.scanGeneration
	roots := append([]string(nil), a.config.Roots...)
	ctx, cancel := context.WithCancel(a.ctx)
	a.scanCancel = cancel
	a.scanning = true
	enabled(a.start, false)
	enabled(a.stop, true)
	a.lastNotice = "正在建立本机元数据快照；界面仍可操作。"
	a.renderStatus()
	events, hwnd, cachePath, appCtx, gate := a.events, a.hwnd, a.cachePath, a.ctx, a.scanGate
	go func() {
		defer cancel()
		select {
		case gate <- struct{}{}:
		case <-ctx.Done():
			return
		}
		defer func() { <-gate }()
		if ctx.Err() != nil {
			return
		}
		for _, root := range roots {
			if _, err := localDirectory(root); err != nil {
				sendEvent(appCtx, events, hwnd, uiEvent{kind: "scan", generation: generation, err: err}, false)
				return
			}
		}
		lastProgress := time.Time{}
		idx, err := index.Scan(ctx, roots, func(progress index.Progress) {
			if time.Since(lastProgress) < 150*time.Millisecond {
				return
			}
			lastProgress = time.Now()
			sendEvent(appCtx, events, hwnd, uiEvent{kind: "progress", generation: generation, progress: progress}, true)
		})
		if ctx.Err() != nil {
			return
		}
		var saveErr error
		if err == nil && cachePath != "" {
			saveErr = idx.SaveContext(ctx, cachePath)
			// 保存已返回后若任务被取消，该快照不再可信：直接丢弃，避免覆盖更新的状态。
			if ctx.Err() != nil {
				return
			}
		}
		sendEvent(appCtx, events, hwnd, uiEvent{kind: "scan", generation: generation, idx: idx, err: err, saveErr: saveErr, updated: time.Now()}, false)
	}()
}

func (a *application) pauseScan() {
	a.autoSync = false
	killTimer.Call(a.hwnd, syncTimer)
	a.scanGeneration++
	if a.scanCancel != nil {
		a.scanCancel()
		a.scanCancel = nil
	}
	a.scanning = false
	enabled(a.start, len(a.config.Roots) > 0)
	enabled(a.stop, false)
}

func (a *application) searchNow() {
	a.searchGeneration++
	if a.idx == nil {
		a.setRows(nil)
		a.lastNotice = "还没有可用索引。添加目录后点击“开始索引”。"
		a.renderStatus()
		return
	}
	query := strings.TrimSpace(text(a.search))
	selected, _, _ := sendMessage.Call(a.types, cbGetCurSel, 0, 0)
	filter := int(int32(selected))
	kinds := []string{"", "file", "folder", "doc", "image", "audio", "video", "archive"}
	kind := ""
	if filter >= 0 && filter < len(kinds) {
		kind = kinds[filter]
	}
	a.lastNotice = "正在搜索已有索引…"
	a.renderStatus()
	request := searchRequest{generation: a.searchGeneration, idx: a.idx, query: query, kind: kind}
	select {
	case a.searchRequests <- request:
	default:
		select {
		case <-a.searchRequests:
		default:
		}
		select {
		case a.searchRequests <- request:
		case <-a.ctx.Done():
		}
	}
}

func (a *application) startSearchWorker() {
	ctx, requests, events, hwnd := a.ctx, a.searchRequests, a.events, a.hwnd
	go func() {
		for {
			select {
			case <-ctx.Done():
				return
			case request := <-requests:
				if ctx.Err() != nil {
					return
				}
				entries, total, err := request.idx.SearchFiltered(request.query, request.kind, resultLimit)
				sendEvent(ctx, events, hwnd, uiEvent{kind: "search", generation: request.generation, entries: entries, total: total, err: err}, false)
			}
		}
	}()
}

func (a *application) handleEvent(event uiEvent) {
	if a.closing {
		return
	}
	switch event.kind {
	case "progress":
		if event.generation != a.scanGeneration || !a.scanning {
			return
		}
		a.lastNotice = fmt.Sprintf("扫描中：已访问 %d 项，跳过 %d 项 · %s", event.progress.Visited, event.progress.Skipped, event.progress.Current)
		a.renderStatus()
	case "scan":
		if event.generation != a.scanGeneration {
			return
		}
		a.scanning = false
		a.scanCancel = nil
		enabled(a.start, len(a.config.Roots) > 0)
		enabled(a.stop, a.autoSync)
		if event.err != nil {
			a.lastNotice = "索引失败，已有缓存仍可使用：" + event.err.Error()
			a.renderStatus()
			return
		}
		// 缓存未持久化时不能替换当前索引：旧索引与旧搜索结果继续可用。
		if a.cachePath != "" && event.saveErr != nil {
			a.lastNotice = "本次索引未提交，保留原索引与搜索结果。"
			a.storageNotice = "缓存未保存：" + event.saveErr.Error()
			a.renderStatus()
			return
		}
		a.idx = event.idx
		a.lastUpdate = event.updated
		a.storageNotice = ""
		a.lastNotice = fmt.Sprintf("索引完成：%d 项。", len(a.idx.Entries))
		if a.cachePath == "" {
			// 启动时已明确降级为仅内存模式：快照可用，但不存在有效缓存范围。
			a.storageNotice = "仅内存索引，退出后丢失；配置与缓存未持久化。"
			a.config.CacheRoots = nil
		} else {
			// 只有真正持久化成功，CacheRoots 才代表下次启动可加载的缓存范围。
			a.config.CacheRoots = append([]string(nil), a.config.Roots...)
			if configErr := a.saveConfiguration(); configErr != nil {
				a.config.CacheRoots = nil
				a.storageNotice = "配置未保存：" + configErr.Error()
			}
		}
		a.renderStatus()
		a.searchNow()
	case "cache":
		if event.generation != a.scanGeneration || a.idx != nil {
			return
		}
		if event.err != nil {
			a.lastNotice = "未加载索引缓存：" + event.err.Error() + "。请手动开始索引。"
			a.renderStatus()
			return
		}
		a.idx = event.idx
		a.lastUpdate = event.updated
		a.lastNotice = "已加载本机缓存，未启动扫描。点击“开始索引”才启用同步。"
		a.renderStatus()
		a.searchNow()
	case "search":
		if event.generation != a.searchGeneration {
			return
		}
		if event.err != nil {
			a.setRows(nil)
			a.lastNotice = "搜索条件有误：" + event.err.Error()
			a.renderStatus()
			return
		}
		a.setRows(event.entries)
		a.lastNotice = fmt.Sprintf("共命中 %d 项，显示 %d 项", event.total, len(event.entries))
		if event.total > resultLimit {
			a.lastNotice += "（最多 2000 项，请缩小搜索条件）"
		}
		a.renderStatus()
	}
}

func (a *application) setRows(entries []index.Entry) {
	sendMessage.Call(a.results, 0x000b, 0, 0)
	sendMessage.Call(a.results, lvmDeleteAll, 0, 0)
	a.rows = entries
	for position, entry := range entries {
		item := listItem{Mask: 1, Item: int32(position), Text: wide(entry.Name)}
		sendMessage.Call(a.results, lvmInsertItem, 0, uintptr(unsafe.Pointer(&item)))
		size := formatSize(entry.Size)
		if entry.IsDir {
			size = "文件夹"
		}
		for sub, value := range []string{entry.Path, size, time.Unix(entry.Modified, 0).Local().Format("2006-01-02 15:04")} {
			item.SubItem = int32(sub + 1)
			item.Text = wide(value)
			sendMessage.Call(a.results, lvmSetItem, 0, uintptr(unsafe.Pointer(&item)))
		}
	}
	sendMessage.Call(a.results, 0x000b, 1, 0)
	user32.NewProc("InvalidateRect").Call(a.results, 0, 1)
	a.selectionButtons()
}

func formatSize(size int64) string {
	if size < 1024 {
		return fmt.Sprintf("%d B", size)
	}
	units := []string{"KB", "MB", "GB", "TB", "PB"}
	value := float64(size) / 1024
	unit := 0
	for value >= 1024 && unit < len(units)-1 {
		value /= 1024
		unit++
	}
	return fmt.Sprintf("%.1f %s", value, units[unit])
}

func (a *application) selectedEntry() (index.Entry, bool) {
	n, _, _ := sendMessage.Call(a.results, lvmGetNextItem, ^uintptr(0), 2)
	position := int(int32(n))
	if position < 0 || position >= len(a.rows) {
		return index.Entry{}, false
	}
	return a.rows[position], true
}

func (a *application) selectionButtons() {
	_, selected := a.selectedEntry()
	enabled(a.open, selected)
	enabled(a.reveal, selected)
	enabled(a.copy, selected)
}

func (a *application) fileAction(action int) {
	entry, selected := a.selectedEntry()
	if !selected {
		return
	}
	if err := localPath(entry.Path); err != nil {
		warn(a.hwnd, err.Error())
		return
	}
	inScope := false
	for _, root := range a.config.Roots {
		if withinRoot(root, entry.Path) {
			inScope = true
			break
		}
	}
	if !inScope {
		warn(a.hwnd, "该项目已不在当前索引范围内，请重新索引。")
		return
	}
	info, err := os.Lstat(entry.Path)
	if err != nil {
		warn(a.hwnd, "所选项目不存在或无法访问，请重新索引。\n"+err.Error())
		return
	}
	if isReparse(info) {
		warn(a.hwnd, "为避免打开范围外目标，不打开链接或重解析点。请直接选择本机目标目录建立索引。")
		return
	}
	resolved, err := filepath.EvalSymlinks(entry.Path)
	if err != nil {
		warn(a.hwnd, "无法验证所选项目的本机路径："+err.Error())
		return
	}
	if err := localPath(resolved); err != nil {
		warn(a.hwnd, err.Error())
		return
	}
	inside := false
	for _, root := range a.config.Roots {
		if withinRoot(root, resolved) {
			inside = true
			break
		}
	}
	if !inside {
		warn(a.hwnd, "项目实际位置已不在当前本机索引范围内，请重新索引。")
		return
	}
	switch action {
	case idOpen:
		err = openPath(a.hwnd, resolved)
	case idReveal:
		err = revealPath(resolved)
	case idCopy:
		err = copyToClipboard(a.hwnd, resolved)
	}
	if err != nil {
		warn(a.hwnd, err.Error())
	}
}

func (a *application) renderStatus() {
	mode := "同步未启动"
	if a.autoSync {
		mode = "30 秒同步已启用"
	}
	if a.scanning {
		mode = "扫描中 · " + mode
	}
	if a.cachePath == "" {
		mode = "仅内存 · 不持久化 · " + mode
	}
	count := 0
	if a.idx != nil {
		count = len(a.idx.Entries)
	}
	updated := ""
	if !a.lastUpdate.IsZero() {
		updated = " · 快照 " + a.lastUpdate.Local().Format("01-02 15:04:05")
	}
	notice := strings.ReplaceAll(strings.ReplaceAll(a.lastNotice+" "+a.storageNotice, "\r", " "), "\n", " ")
	putText(a.status, fmt.Sprintf("%s · 索引 %d 项%s · %s", mode, count, updated, notice))
}

func (a *application) shutdown() {
	if a.closing {
		return
	}
	a.closing = true
	killTimer.Call(a.hwnd, searchTimer)
	killTimer.Call(a.hwnd, syncTimer)
	if a.scanCancel != nil {
		a.scanCancel()
	}
	a.cancel()
}

func isReparse(info os.FileInfo) bool {
	if info.Mode()&os.ModeSymlink != 0 {
		return true
	}
	switch data := info.Sys().(type) {
	case *syscall.Win32FileAttributeData:
		return data.FileAttributes&0x0400 != 0
	case *syscall.ByHandleFileInformation:
		return data.FileAttributes&0x0400 != 0
	}
	return false
}

func localPath(path string) error {
	if strings.ContainsRune(path, 0) || strings.HasPrefix(path, `\\`) || len(path) < 3 || path[1] != ':' || (path[2] != '\\' && path[2] != '/') || !((path[0] >= 'A' && path[0] <= 'Z') || (path[0] >= 'a' && path[0] <= 'z')) {
		return errors.New("仅支持带盘符的本机目录，不支持 UNC、设备路径或网络共享")
	}
	root := path[:2] + `\`
	driveType, _, _ := getDriveType.Call(uintptr(unsafe.Pointer(wide(root))))
	if driveType == 4 {
		return errors.New("不支持网络映射盘；请改选本机目录")
	}
	if driveType == 0 || driveType == 1 {
		return errors.New("所选盘符不可用，请选择可访问的本机目录")
	}
	volume := make([]uint16, 32768)
	ok, _, err := getVolumePathName.Call(uintptr(unsafe.Pointer(wide(path))), uintptr(unsafe.Pointer(&volume[0])), uintptr(len(volume)))
	if ok == 0 {
		return nativeError("验证本机卷路径", err)
	}
	driveType, _, _ = getDriveType.Call(uintptr(unsafe.Pointer(&volume[0])))
	if driveType == 4 || driveType == 0 || driveType == 1 {
		return errors.New("目录所在卷不是可用本机磁盘，不支持网络映射或共享")
	}
	return nil
}

func localDirectory(path string) (string, error) {
	if err := localPath(path); err != nil {
		return "", err
	}
	absolute, err := filepath.Abs(filepath.Clean(path))
	if err != nil {
		return "", fmt.Errorf("目录路径无效：%w", err)
	}
	resolved, err := filepath.EvalSymlinks(absolute)
	if err != nil {
		return "", fmt.Errorf("目录不存在或无法访问：%w", err)
	}
	if err := localPath(resolved); err != nil {
		return "", err
	}
	info, err := os.Stat(resolved)
	if err != nil {
		return "", fmt.Errorf("读取目录失败：%w", err)
	}
	if !info.IsDir() {
		return "", errors.New("请选择本机目录，而不是单个文件")
	}
	return filepath.Clean(resolved), nil
}

func withinRoot(root, path string) bool {
	relative, err := filepath.Rel(strings.ToLower(filepath.Clean(root)), strings.ToLower(filepath.Clean(path)))
	return err == nil && relative != ".." && !strings.HasPrefix(relative, ".."+string(filepath.Separator)) && !filepath.IsAbs(relative)
}

func (a *application) loadConfiguration() {
	base, err := os.UserConfigDir()
	if err != nil {
		a.startupNotice = "无法读取当前用户配置目录；本次仅使用内存。"
		return
	}
	if err := localPath(base); err != nil {
		a.startupNotice = "当前用户配置目录不在可用本机磁盘；为保持离线，本次仅使用内存。"
		return
	}
	resolvedBase, err := filepath.EvalSymlinks(base)
	if err != nil {
		a.startupNotice = "无法验证当前用户配置目录；本次仅使用内存：" + err.Error()
		return
	}
	if err := localPath(resolvedBase); err != nil {
		a.startupNotice = "当前用户配置目录实际位于网络位置；本次仅使用内存。"
		return
	}
	base = filepath.Join(base, "OilFindIntranet")
	if err := os.MkdirAll(base, 0700); err != nil {
		a.startupNotice = "无法创建当前用户配置目录；本次仅使用内存：" + err.Error()
		return
	}
	base, err = filepath.EvalSymlinks(base)
	if err != nil {
		a.startupNotice = "无法验证应用配置目录；本次仅使用内存。"
		return
	}
	if err := localPath(base); err != nil {
		a.startupNotice = "应用配置目录不在本机磁盘；本次仅使用内存。"
		return
	}
	a.configPath, a.cachePath = filepath.Join(base, "config.json"), filepath.Join(base, "index.json")
	if info, statErr := os.Lstat(a.configPath); statErr == nil && isReparse(info) {
		a.startupNotice = "目录配置是链接，未加载；请重新添加本机目录。"
		return
	}
	file, err := os.Open(a.configPath)
	if errors.Is(err, os.ErrNotExist) {
		return
	}
	if err != nil {
		a.startupNotice = "读取目录配置失败；请重新添加本机目录：" + err.Error()
		return
	}
	defer file.Close()
	var config configuration
	decoder := json.NewDecoder(io.LimitReader(file, 1024*1024))
	if err := decoder.Decode(&config); err != nil {
		a.startupNotice = "目录配置损坏；不会自动扫描，请重新添加目录。"
		return
	}
	if len(config.Roots) > 64 {
		a.startupNotice = "目录配置超出安全上限；请重新添加目录。"
		return
	}
	for _, root := range config.Roots {
		validated, err := localDirectory(root)
		if err != nil {
			a.startupNotice = "已跳过无法访问或非本机的已保存目录；不会自动扫描。"
			continue
		}
		duplicate := false
		for _, existing := range a.config.Roots {
			if strings.EqualFold(existing, validated) {
				duplicate = true
				break
			}
		}
		if !duplicate {
			a.config.Roots = append(a.config.Roots, validated)
		}
	}
	if sameRoots(config.Roots, a.config.Roots) && sameRoots(config.CacheRoots, a.config.Roots) {
		a.config.CacheRoots = append([]string(nil), a.config.Roots...)
	}
}

func sameRoots(left, right []string) bool {
	if len(left) != len(right) {
		return false
	}
	for n := range left {
		if !strings.EqualFold(filepath.Clean(left[n]), filepath.Clean(right[n])) {
			return false
		}
	}
	return true
}

func sameRootSet(left, right []string) bool {
	if len(left) != len(right) {
		return false
	}
	for _, root := range left {
		found := false
		for _, candidate := range right {
			if strings.EqualFold(filepath.Clean(root), filepath.Clean(candidate)) {
				found = true
				break
			}
		}
		if !found {
			return false
		}
	}
	return true
}

func (a *application) saveConfiguration() error {
	if a.configPath == "" {
		return errors.New("当前用户配置目录不可用")
	}
	data, err := json.MarshalIndent(a.config, "", "  ")
	if err != nil {
		return err
	}
	temporary, err := os.CreateTemp(filepath.Dir(a.configPath), "config-*.tmp")
	if err != nil {
		return err
	}
	name := temporary.Name()
	defer os.Remove(name)
	if _, err := temporary.Write(data); err != nil {
		temporary.Close()
		return err
	}
	if err := temporary.Close(); err != nil {
		return err
	}
	return os.Rename(name, a.configPath)
}

func (a *application) loadCache() {
	if a.cachePath == "" || len(a.config.Roots) == 0 || !sameRoots(a.config.Roots, a.config.CacheRoots) {
		return
	}
	ctx, events, hwnd, path, generation := a.ctx, a.events, a.hwnd, a.cachePath, a.scanGeneration
	roots := append([]string(nil), a.config.Roots...)
	go func() {
		info, err := os.Lstat(path)
		if err != nil {
			sendEvent(ctx, events, hwnd, uiEvent{kind: "cache", generation: generation, err: err}, false)
			return
		}
		if isReparse(info) || !info.Mode().IsRegular() {
			sendEvent(ctx, events, hwnd, uiEvent{kind: "cache", generation: generation, err: errors.New("索引缓存不是普通本机文件")}, false)
			return
		}
		idx, err := index.Load(path)
		var updated time.Time
		if err == nil && !sameRootSet(idx.Roots, roots) {
			err = errors.New("缓存目录与当前索引范围不一致")
		}
		if err == nil {
			if stat, statErr := os.Stat(path); statErr == nil {
				updated = stat.ModTime()
			}
			entries := make([]index.Entry, 0, len(idx.Entries))
			for _, entry := range idx.Entries {
				if ctx.Err() != nil {
					return
				}
				for _, root := range roots {
					if withinRoot(root, entry.Path) {
						entries = append(entries, entry)
						break
					}
				}
			}
			idx = &index.Index{Entries: entries}
		}
		sendEvent(ctx, events, hwnd, uiEvent{kind: "cache", generation: generation, idx: idx, err: err, updated: updated}, false)
	}()
}
