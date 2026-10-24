package index_test

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"testing"

	index "oilfind-intranet/windows/internal/index"
)

func assertEntryMetadata(t *testing.T, got, want []index.Entry) {
	t.Helper()
	if len(got) != len(want) {
		t.Fatalf("entry count=%d, want %d", len(got), len(want))
	}
	for i, entry := range got {
		expected := want[i]
		if entry.Path != expected.Path || entry.Name != expected.Name || entry.Size != expected.Size || entry.Modified != expected.Modified || entry.IsDir != expected.IsDir {
			t.Fatalf("entry %d=%+v, want %+v", i, entry, expected)
		}
	}
}

func TestCacheRoundTripRebuildsSearchMetadata(t *testing.T) {
	idx := searchFixture(t)
	cacheDir := testTempDir(t)
	filename := filepath.Join(cacheDir, "index.json")
	if err := idx.Save(filename); err != nil {
		t.Fatal(err)
	}
	loaded, err := index.Load(filename)
	if err != nil {
		t.Fatal(err)
	}
	assertEntryMetadata(t, loaded.Entries, idx.Entries)
	assertSearchNames(t, loaded, "HUIYI", []string{"会议纪要.docx"})
	assertSearchNames(t, loaded, "hyjy", []string{"会议纪要.docx"})
	assertSearchNames(t, loaded, "report !archive ext:pdf", []string{"Report Final.PDF"})
	files, err := os.ReadDir(cacheDir)
	if err != nil {
		t.Fatal(err)
	}
	if len(files) != 1 || files[0].Name() != "index.json" {
		t.Fatalf("temporary cache files were left behind: %v", files)
	}
}

func TestCacheAtomicReplacement(t *testing.T) {
	root := testTempDir(t)
	filename := filepath.Join(root, "cache", "index.json")
	old := &index.Index{Entries: []index.Entry{{Path: filepath.Join(root, "old.txt"), Name: "old.txt", Size: 3}}}
	newIndex := &index.Index{Entries: []index.Entry{{Path: filepath.Join(root, "new.txt"), Name: "new.txt", Size: 5}}}
	if err := old.Save(filename); err != nil {
		t.Fatal(err)
	}
	if err := newIndex.Save(filename); err != nil {
		t.Fatal(err)
	}
	loaded, err := index.Load(filename)
	if err != nil {
		t.Fatal(err)
	}
	assertEntryMetadata(t, loaded.Entries, newIndex.Entries)
	files, err := os.ReadDir(filepath.Dir(filename))
	if err != nil {
		t.Fatal(err)
	}
	if len(files) != 1 {
		t.Fatalf("cache replacement left temporary files: %v", files)
	}
}

func TestCacheCancellationPreservesPreviousIndex(t *testing.T) {
	root := testTempDir(t)
	filename := filepath.Join(root, "cache", "index.json")
	selected := filepath.Join(root, "selected")
	writeFixtureFile(t, filepath.Join(selected, "new.txt"), "new sample")
	old := &index.Index{Entries: []index.Entry{{Path: filepath.Join(root, "old.txt"), Name: "old.txt", Size: 3}}}
	if err := old.Save(filename); err != nil {
		t.Fatal(err)
	}
	before, err := os.ReadFile(filename)
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	result, err := index.Scan(ctx, []string{selected}, nil)
	if !errors.Is(err, context.Canceled) || result != nil {
		t.Fatalf("canceled scan returned a publishable index: index=%v err=%v", result, err)
	}
	after, err := os.ReadFile(filename)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(before, after) {
		t.Fatal("canceled scan changed previous cache")
	}
	loaded, err := index.Load(filename)
	if err != nil {
		t.Fatal(err)
	}
	assertEntryMetadata(t, loaded.Entries, old.Entries)
}

func TestCacheFailedWritePreservesPreviousIndex(t *testing.T) {
	if runtime.GOOS == "windows" || os.Geteuid() == 0 {
		t.Skip("permission fixture requires non-root Unix execution")
	}
	root := testTempDir(t)
	cacheDir := filepath.Join(root, "cache")
	filename := filepath.Join(cacheDir, "index.json")
	old := &index.Index{Entries: []index.Entry{{Path: filepath.Join(root, "old.txt"), Name: "old.txt"}}}
	if err := old.Save(filename); err != nil {
		t.Fatal(err)
	}
	before, err := os.ReadFile(filename)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(cacheDir, 0o500); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.Chmod(cacheDir, 0o700) })
	newIndex := &index.Index{Entries: []index.Entry{{Path: filepath.Join(root, "new.txt"), Name: "new.txt"}}}
	if err := newIndex.Save(filename); err == nil {
		t.Fatal("Save succeeded despite an unwritable cache directory")
	}
	after, err := os.ReadFile(filename)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(before, after) {
		t.Fatal("failed save changed previous cache")
	}
}

func TestCacheRejectsCorruptionAndInvalidVersion(t *testing.T) {
	root := testTempDir(t)
	valid := filepath.Join(root, "valid.json")
	idx := &index.Index{Entries: []index.Entry{{Path: filepath.Join(root, "sample.txt"), Name: "sample.txt", Size: 1}}}
	if err := idx.Save(valid); err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(valid)
	if err != nil {
		t.Fatal(err)
	}
	var payload map[string]json.RawMessage
	if err := json.Unmarshal(data, &payload); err != nil {
		t.Fatal(err)
	}
	var version int
	if err := json.Unmarshal(payload["version"], &version); err != nil {
		t.Fatal(err)
	}
	changedPayload := func(key string, value any) []byte {
		copyPayload := make(map[string]json.RawMessage, len(payload))
		for k, v := range payload {
			copyPayload[k] = v
		}
		encoded, err := json.Marshal(value)
		if err != nil {
			t.Fatal(err)
		}
		copyPayload[key] = encoded
		encoded, err = json.Marshal(copyPayload)
		if err != nil {
			t.Fatal(err)
		}
		return encoded
	}
	tooManyRoots := make([]string, 65)
	for i := range tooManyRoots {
		tooManyRoots[i] = root
	}
	cases := []struct {
		name string
		data []byte
	}{
		{"empty", nil},
		{"garbage", []byte("not a cache")},
		{"truncated", data[:len(data)/2]},
		{"future-version", changedPayload("version", version+1)},
		{"missing-version", []byte(`{"entries":[]}`)},
		{"trailing-json", append(append([]byte(nil), data...), []byte(`{"extra":true}`)...)},
		{"too-many-roots", changedPayload("roots", tooManyRoots)},
		{"missing-path", changedPayload("entries", []index.Entry{{Name: "sample.txt"}})},
		{"missing-name", changedPayload("entries", []index.Entry{{Path: filepath.Join(root, "sample.txt")}})},
		{"negative-size", changedPayload("entries", []index.Entry{{Path: filepath.Join(root, "sample.txt"), Name: "sample.txt", Size: -1}})},
		{"nul-path", changedPayload("entries", []index.Entry{{Path: root + "\x00bad", Name: "bad"}})},
		{"nul-name", changedPayload("entries", []index.Entry{{Path: filepath.Join(root, "sample.txt"), Name: "bad\x00name"}})},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			filename := filepath.Join(root, tc.name+".json")
			if err := os.WriteFile(filename, tc.data, 0o600); err != nil {
				t.Fatal(err)
			}
			if loaded, err := index.Load(filename); err == nil || loaded != nil {
				t.Fatalf("Load accepted corrupted cache: index=%v err=%v", loaded, err)
			}
		})
	}
	if loaded, err := index.Load(filepath.Join(root, "missing.json")); err == nil || loaded != nil {
		t.Fatalf("Load accepted a nonexistent cache: index=%v err=%v", loaded, err)
	}
}

func TestCacheRejectsOversizedFileBeforeDecoding(t *testing.T) {
	filename := filepath.Join(testTempDir(t), "oversized.json")
	file, err := os.Create(filename)
	if err != nil {
		t.Fatal(err)
	}
	if err := file.Truncate((512 << 20) + 1); err != nil {
		_ = file.Close()
		t.Skipf("sparse size fixture unavailable: %v", err)
	}
	if err := file.Close(); err != nil {
		t.Fatal(err)
	}
	if loaded, err := index.Load(filename); err == nil || loaded != nil {
		t.Fatalf("Load accepted oversized cache: index=%v err=%v", loaded, err)
	}
}
