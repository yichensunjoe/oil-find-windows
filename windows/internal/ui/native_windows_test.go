//go:build windows

package ui

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"
	"unsafe"

	"oilfind/internal/index"
)

func TestNativeStructureLayouts(t *testing.T) {
	if unsafe.Sizeof(uintptr(0)) != 8 {
		t.Skip("此预览版仅交付 64 位 Windows")
	}
	checks := []struct {
		name      string
		got, want uintptr
	}{
		{"MSG", unsafe.Sizeof(winMessage{}), 48},
		{"WNDCLASSEXW", unsafe.Sizeof(wndClass{}), 80},
		{"NMHDR", unsafe.Sizeof(notifyHeader{}), 24},
		{"LVCOLUMNW", unsafe.Sizeof(listColumn{}), 56},
		{"LVITEMW", unsafe.Sizeof(listItem{}), 88},
		{"BROWSEINFOW", unsafe.Sizeof(browseInfo{}), 64},
		{"MSG.wParam", unsafe.Offsetof(winMessage{}.WParam), 16},
		{"LVITEMW.pszText", unsafe.Offsetof(listItem{}.Text), 24},
		{"LVITEMW.lParam", unsafe.Offsetof(listItem{}.Param), 40},
		{"LVCOLUMNW.pszText", unsafe.Offsetof(listColumn{}.Text), 16},
	}
	for _, check := range checks {
		if check.got != check.want {
			t.Errorf("%s 布局为 %d，预期 %d", check.name, check.got, check.want)
		}
	}
}

func TestNetworkAndDevicePathsRejectedBeforeIO(t *testing.T) {
	for _, path := range []string{`\\server\share`, `\\?\C:\files`, `\\.\pipe\oilfind`, `//server/share`, `relative`, `C:relative`, "C:\\files\x00hidden"} {
		if err := localPath(path); err == nil {
			t.Errorf("应拒绝路径 %q", path)
		}
	}
}

func TestScopeContainment(t *testing.T) {
	for _, check := range []struct {
		root, path string
		want       bool
	}{
		{`C:\Work`, `C:\Work\报告.docx`, true},
		{`C:\Work`, `c:\work\合同.pdf`, true},
		{`C:\Work`, `C:\Workspace\other.txt`, false},
		{`C:\Work`, `D:\Work\other.txt`, false},
		{`C:\Work`, `C:\Work\..\Secrets\key.txt`, false},
	} {
		if got := withinRoot(check.root, check.path); got != check.want {
			t.Errorf("范围 %q 项目 %q：得到 %v，预期 %v", check.root, check.path, got, check.want)
		}
	}
}

func TestSnapshotSaveFailurePreservesOldState(t *testing.T) {
	old := &index.Index{Entries: []index.Entry{{Path: `C:\Work\old.txt`, Name: "old.txt"}}}
	built := time.Unix(100, 0)
	a := &application{idx: old, rows: old.Entries, cachePath: `C:\Cache\index.json`, scanGeneration: 9, lastUpdate: built, config: configuration{Roots: []string{`C:\Work`}, CacheRoots: []string{`C:\Work`}}}
	a.handleEvent(uiEvent{kind: "scan", generation: 9, idx: &index.Index{}, saveErr: errors.New("磁盘不可写"), updated: time.Unix(200, 0)})
	if a.idx != old || len(a.rows) != 1 || a.rows[0].Name != "old.txt" || !a.lastUpdate.Equal(built) || len(a.config.CacheRoots) != 1 {
		t.Fatal("保存失败应保留旧索引、结果、时间与缓存范围")
	}
	if !strings.Contains(a.storageNotice, "磁盘不可写") {
		t.Fatal("保存错误必须保留在状态提示中")
	}
}

func TestMemoryOnlySnapshotIsExplicit(t *testing.T) {
	snapshot := &index.Index{Entries: []index.Entry{{Path: `C:\Work\memory.txt`, Name: "memory.txt"}}}
	a := &application{ctx: context.Background(), searchRequests: make(chan searchRequest, 1), scanGeneration: 3}
	a.handleEvent(uiEvent{kind: "scan", generation: 3, idx: snapshot, saveErr: errors.New("配置目录不可用")})
	if a.idx != snapshot || len(a.config.CacheRoots) != 0 || !strings.Contains(a.storageNotice, "仅内存") {
		t.Fatal("显式内存模式应采用快照、不给出有效持久缓存范围，并持续提示仅内存")
	}
}

func TestStaleSnapshotIsIgnored(t *testing.T) {
	old := &index.Index{}
	a := &application{idx: old, scanGeneration: 7}
	a.handleEvent(uiEvent{kind: "scan", generation: 6, idx: &index.Index{}})
	if a.idx != old {
		t.Fatal("旧扫描 generation 不得替换当前索引")
	}
}

func TestSizeFormatting(t *testing.T) {
	for _, check := range []struct {
		size int64
		want string
	}{
		{0, "0 B"}, {1023, "1023 B"}, {1024, "1.0 KB"}, {1 << 20, "1.0 MB"}, {1 << 40, "1.0 TB"},
	} {
		if got := formatSize(check.size); got != check.want {
			t.Errorf("大小 %d 得到 %q，预期 %q", check.size, got, check.want)
		}
	}
}
