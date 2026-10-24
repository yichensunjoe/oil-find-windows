package index_test

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"testing"

	index "oilfind-intranet/windows/internal/index"
)

func testTempDir(t *testing.T) string {
	t.Helper()
	root := t.TempDir()
	canonical, err := filepath.EvalSymlinks(root)
	if err != nil {
		t.Fatal(err)
	}
	return canonical
}

func writeFixtureFile(t *testing.T, path string, data string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(data), 0o600); err != nil {
		t.Fatal(err)
	}
}

func indexedPaths(t *testing.T, idx *index.Index) map[string]index.Entry {
	t.Helper()
	if idx == nil {
		t.Fatal("Scan returned a nil index without an error")
	}
	paths := make(map[string]index.Entry, len(idx.Entries))
	for _, entry := range idx.Entries {
		path := filepath.Clean(entry.Path)
		if _, exists := paths[path]; exists {
			t.Fatalf("duplicate indexed path: %q", path)
		}
		paths[path] = entry
	}
	return paths
}

func TestScanSelectedRootMetadataAndOverlapDeduplication(t *testing.T) {
	root := testTempDir(t)
	child := filepath.Join(root, "documents")
	file := filepath.Join(child, "会议纪要.PDF")
	writeFixtureFile(t, file, "fixture metadata only")
	writeFixtureFile(t, filepath.Join(root, "a.txt"), "sample")
	var reports []index.Progress
	idx, err := index.Scan(context.Background(), []string{root, child, root, filepath.Join(child, ".")}, func(p index.Progress) {
		reports = append(reports, p)
	})
	if err != nil {
		t.Fatal(err)
	}
	paths := indexedPaths(t, idx)
	entry, ok := paths[file]
	if !ok {
		t.Fatalf("selected file was not indexed: %v", paths)
	}
	info, err := os.Stat(file)
	if err != nil {
		t.Fatal(err)
	}
	if entry.Name != "会议纪要.PDF" || entry.IsDir || entry.Size != info.Size() || entry.Modified != info.ModTime().Unix() {
		t.Fatalf("unexpected metadata: %+v, file info=%v", entry, info)
	}
	if directory, ok := paths[child]; !ok || !directory.IsDir {
		t.Fatalf("directory metadata missing: %+v", directory)
	}
	if len(reports) == 0 {
		t.Fatal("Scan did not report progress")
	}
	previousVisited := 0
	for _, p := range reports {
		if p.Visited < previousVisited || p.Skipped < 0 {
			t.Fatalf("invalid progress: %+v", p)
		}
		previousVisited = p.Visited
	}
}

func TestScanSkipsDependencySystemAndPrivateDirectories(t *testing.T) {
	root := testTempDir(t)
	// Project-internal and private runtime folders are skipped at any depth.
	blocked := []string{"node_modules", ".git", ".claude", ".codex", ".gemini", ".qwen", ".windsurf", ".workbuddy", ".workbuddy-ai", ".freebuff", ".codegraph", ".reasonix", ".pnpm-store"}
	for _, name := range blocked {
		writeFixtureFile(t, filepath.Join(root, name, "must-not-index.txt"), "isolated fixture")
	}
	visible := filepath.Join(root, "documents", "visible.txt")
	writeFixtureFile(t, visible, "visible")
	// Volume-root-only system names must stay searchable when they are nested
	// inside a user-selected project (D:\code\myproject\windows, D:\work\tmp).
	nested := []string{"windows", "tmp", ".tmp", "Program Files", "System Volume Information", "$RECYCLE.BIN"}
	for _, name := range nested {
		writeFixtureFile(t, filepath.Join(root, "project", name, "nested.txt"), "ordinary fixture")
	}
	idx, err := index.Scan(context.Background(), []string{root}, nil)
	if err != nil {
		t.Fatal(err)
	}
	paths := indexedPaths(t, idx)
	if _, ok := paths[visible]; !ok {
		t.Fatal("ordinary selected descendant was skipped")
	}
	for _, name := range blocked {
		if _, ok := paths[filepath.Join(root, name, "must-not-index.txt")]; ok {
			t.Errorf("scanned excluded directory %q", name)
		}
	}
	for _, name := range nested {
		if _, ok := paths[filepath.Join(root, "project", name, "nested.txt")]; !ok {
			t.Errorf("nested ordinary directory %q was hidden by a volume-root-only rule", name)
		}
	}
}

func TestScanNeverFollowsSymlinks(t *testing.T) {
	fixture := testTempDir(t)
	root := filepath.Join(fixture, "selected")
	external := filepath.Join(fixture, "outside")
	outsideFile := filepath.Join(external, "outside-secret.txt")
	writeFixtureFile(t, outsideFile, "temporary fixture only")
	writeFixtureFile(t, filepath.Join(external, "nested", "nested-secret.txt"), "temporary fixture only")
	writeFixtureFile(t, filepath.Join(root, "safe.txt"), "safe")
	link := filepath.Join(root, "linked-directory")
	if err := os.Symlink(external, link); err != nil {
		t.Skipf("symlink creation unavailable: %v", err)
	}
	fileLink := filepath.Join(root, "linked-file.txt")
	if err := os.Symlink(outsideFile, fileLink); err != nil {
		t.Skipf("file symlink creation unavailable: %v", err)
	}
	idx, err := index.Scan(context.Background(), []string{root}, nil)
	if err != nil {
		t.Fatal(err)
	}
	paths := indexedPaths(t, idx)
	for _, forbidden := range []string{outsideFile, link, fileLink, filepath.Join(link, "outside-secret.txt")} {
		if _, ok := paths[forbidden]; ok {
			t.Errorf("symlink path or target was indexed: %q", forbidden)
		}
	}
	for _, selected := range []string{link, filepath.Join(link, "nested")} {
		idx, err = index.Scan(context.Background(), []string{selected}, nil)
		if err == nil {
			paths = indexedPaths(t, idx)
			for _, entry := range paths {
				if !entry.IsDir {
					t.Errorf("explicit symlink root or ancestor bypassed exclusion: %+v", entry)
				}
			}
		}
	}
}

func TestScanNeverFollowsWindowsJunctions(t *testing.T) {
	if runtime.GOOS != "windows" {
		t.Skip("Windows junction behavior")
	}
	fixture := testTempDir(t)
	root := filepath.Join(fixture, "selected")
	external := filepath.Join(fixture, "outside")
	writeFixtureFile(t, filepath.Join(root, "safe.txt"), "safe")
	writeFixtureFile(t, filepath.Join(external, "outside-secret.txt"), "fixture")
	junction := filepath.Join(root, "junction")
	output, err := exec.Command("cmd.exe", "/c", "mklink", "/J", junction, external).CombinedOutput()
	if err != nil {
		t.Skipf("junction creation unavailable: %v: %s", err, output)
	}
	idx, err := index.Scan(context.Background(), []string{root}, nil)
	if err != nil {
		t.Fatal(err)
	}
	paths := indexedPaths(t, idx)
	if _, ok := paths[junction]; ok {
		t.Fatal("junction itself was indexed")
	}
	if _, ok := paths[filepath.Join(junction, "outside-secret.txt")]; ok {
		t.Fatal("junction target was scanned")
	}
	idx, err = index.Scan(context.Background(), []string{junction}, nil)
	if err == nil {
		for _, entry := range indexedPaths(t, idx) {
			if !entry.IsDir {
				t.Fatalf("explicit junction root bypassed exclusion: %+v", entry)
			}
		}
	}
}

func TestScanRejectsProtectedAncestorsAndNonDirectoryRoots(t *testing.T) {
	root := testTempDir(t)
	protected := filepath.Join(root, ".codex", "nested")
	writeFixtureFile(t, filepath.Join(protected, "fixture.txt"), "temporary only")
	plainFile := filepath.Join(root, "plain.txt")
	writeFixtureFile(t, plainFile, "file root must be rejected")
	for _, selected := range []string{filepath.Dir(protected), protected, plainFile, "relative-fixture"} {
		if _, err := index.Scan(context.Background(), []string{selected}, nil); err == nil {
			t.Errorf("Scan accepted protected or non-directory root: %q", selected)
		}
	}
	tooManyRoots := make([]string, 65)
	for i := range tooManyRoots {
		tooManyRoots[i] = root
	}
	if _, err := index.Scan(context.Background(), tooManyRoots, nil); err == nil {
		t.Fatal("Scan accepted more than 64 supplied roots")
	}
}

func TestScanRejectsMissingSelectionAndNetworkRoots(t *testing.T) {
	for _, roots := range [][]string{nil, {}, {""}, {`\\server\share`}, {`//server/share`}, {`\\?\UNC\server\share`}} {
		t.Run(fmt.Sprintf("%q", roots), func(t *testing.T) {
			if _, err := index.Scan(context.Background(), roots, nil); err == nil {
				t.Fatalf("Scan accepted invalid or network roots: %q", roots)
			}
		})
	}
}

func TestScanCancellationNeverReturnsPartialIndex(t *testing.T) {
	root := testTempDir(t)
	for i := 0; i < 100; i++ {
		writeFixtureFile(t, filepath.Join(root, fmt.Sprintf("sample-%03d.txt", i)), "temporary")
	}
	t.Run("already-canceled", func(t *testing.T) {
		ctx, cancel := context.WithCancel(context.Background())
		cancel()
		idx, err := index.Scan(ctx, []string{root}, nil)
		if !errors.Is(err, context.Canceled) || idx != nil {
			t.Fatalf("Scan canceled: index=%v err=%v", idx, err)
		}
	})
	t.Run("cancel-from-progress", func(t *testing.T) {
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()
		idx, err := index.Scan(ctx, []string{root}, func(p index.Progress) {
			if p.Visited >= 1 {
				cancel()
			}
		})
		if !errors.Is(err, context.Canceled) || idx != nil {
			t.Fatalf("Scan delivered partial index after cancellation: index-nil=%t err=%v", idx == nil, err)
		}
	})
}

func TestScanSkipsUnreadableDirectoryWithProgress(t *testing.T) {
	if runtime.GOOS == "windows" || os.Geteuid() == 0 {
		t.Skip("permission fixture requires non-root Unix execution")
	}
	root := testTempDir(t)
	denied := filepath.Join(root, "denied")
	writeFixtureFile(t, filepath.Join(denied, "private.txt"), "fixture")
	writeFixtureFile(t, filepath.Join(root, "visible.txt"), "visible")
	if err := os.Chmod(denied, 0); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.Chmod(denied, 0o700) })
	var skipped int
	idx, err := index.Scan(context.Background(), []string{root}, func(p index.Progress) {
		if p.Skipped > skipped {
			skipped = p.Skipped
		}
	})
	if err != nil {
		t.Fatal(err)
	}
	paths := indexedPaths(t, idx)
	if _, ok := paths[filepath.Join(denied, "private.txt")]; ok {
		t.Fatal("unreadable descendant was indexed")
	}
	if _, ok := paths[filepath.Join(root, "visible.txt")]; !ok {
		t.Fatal("unreadable descendant prevented scanning siblings")
	}
	if skipped < 1 {
		t.Fatal("unreadable directory was not reported as skipped")
	}
}
