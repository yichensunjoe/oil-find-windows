package index

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
)

func TestSaveContextCanceledPreservesCache(t *testing.T) {
	root, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	cache := filepath.Join(root, "index.json")
	original := &Index{Entries: []Entry{{Path: filepath.Join(root, "safe.txt"), Name: "safe.txt"}}}
	if err := original.Save(cache); err != nil {
		t.Fatal(err)
	}
	before, err := os.ReadFile(cache)
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	replacement := &Index{Entries: []Entry{{Path: filepath.Join(root, "new.txt"), Name: "new.txt"}}}
	if err := replacement.SaveContext(ctx, cache); !errors.Is(err, context.Canceled) {
		t.Fatalf("expected cancellation, got %v", err)
	}
	after, err := os.ReadFile(cache)
	if err != nil {
		t.Fatal(err)
	}
	if string(before) != string(after) {
		t.Fatal("canceled save replaced original cache")
	}
}

func TestLoadRejectsSymlinkCache(t *testing.T) {
	root, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	cache := filepath.Join(root, "index.json")
	if err := (&Index{}).Save(cache); err != nil {
		t.Fatal(err)
	}
	link := filepath.Join(root, "linked.json")
	if err := os.Symlink(cache, link); err != nil {
		t.Skipf("symlink unavailable: %v", err)
	}
	if _, err := Load(link); err == nil {
		t.Fatal("symlink cache accepted")
	}
}

func TestSizeRejectsNonFiniteNumbers(t *testing.T) {
	for _, query := range []string{"size:NaN", "size:Inf", "size:Infinity", "size:9223372036854775808"} {
		if _, _, err := (&Index{}).Search(query, 10); err == nil {
			t.Fatalf("accepted invalid numeric condition %q", query)
		}
	}
}

// stepContext reports cancellation only from the second Err check onwards, which
// simulates Ctrl+C arriving after the scan finished but while the cache temp
// file is still being written.
type stepContext struct {
	context.Context
	mu    sync.Mutex
	calls int
}

func (c *stepContext) Err() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.calls++
	if c.calls > 1 {
		return context.Canceled
	}
	return nil
}

// Mirrors the CLI flow: a scan result must never replace the cache when the
// signal arrives before the commit point.
func TestSaveContextCanceledDuringCommitKeepsPreviousCache(t *testing.T) {
	root, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	cacheDir := filepath.Join(root, "cache")
	cache := filepath.Join(cacheDir, "index.json")
	original := &Index{Entries: []Entry{{Path: filepath.Join(root, "old.txt"), Name: "old.txt"}}}
	if err := original.Save(cache); err != nil {
		t.Fatal(err)
	}
	before, err := os.ReadFile(cache)
	if err != nil {
		t.Fatal(err)
	}
	replacement := &Index{Entries: []Entry{{Path: filepath.Join(root, "new.txt"), Name: "new.txt"}}}
	if err := replacement.SaveContext(&stepContext{Context: context.Background()}, cache); !errors.Is(err, context.Canceled) {
		t.Fatalf("expected cancellation before commit, got %v", err)
	}
	after, err := os.ReadFile(cache)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(before, after) {
		t.Fatal("canceled commit replaced the previous cache")
	}
	files, err := os.ReadDir(cacheDir)
	if err != nil {
		t.Fatal(err)
	}
	if len(files) != 1 || files[0].Name() != "index.json" {
		t.Fatalf("canceled commit left temporary files behind: %v", files)
	}
	loaded, err := Load(cache)
	if err != nil {
		t.Fatal(err)
	}
	if len(loaded.Entries) != 1 || loaded.Entries[0].Name != "old.txt" {
		t.Fatalf("previous cache is no longer loadable: %+v", loaded.Entries)
	}
}

func TestSaveRejectsEntryCountOverLimit(t *testing.T) {
	if testing.Short() {
		t.Skip("allocates a MaxEntries+1 slice header array")
	}
	root, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	cache := filepath.Join(root, "index.json")
	original := &Index{Entries: []Entry{{Path: filepath.Join(root, "old.txt"), Name: "old.txt"}}}
	if err := original.Save(cache); err != nil {
		t.Fatal(err)
	}
	before, err := os.ReadFile(cache)
	if err != nil {
		t.Fatal(err)
	}
	// Length only; the limit is checked before any entry is encoded.
	oversized := &Index{Entries: make([]Entry, 0, MaxEntries+1)}
	oversized.Entries = oversized.Entries[:MaxEntries+1]
	if err := oversized.Save(cache); err == nil {
		t.Fatal("Save accepted an index above the entry limit")
	}
	after, err := os.ReadFile(cache)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(before, after) {
		t.Fatal("rejected save changed the previous cache")
	}
}

func TestLoadRejectsDirectoryAsCache(t *testing.T) {
	root, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	dir := filepath.Join(root, "index.json")
	if err := os.Mkdir(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	if loaded, err := Load(dir); err == nil || loaded != nil {
		t.Fatalf("directory accepted as cache: index=%v err=%v", loaded, err)
	}
}

// The 512MiB cap is a package constant with no injection point, so the encoder
// sink is exercised directly instead of materialising a half-gigabyte payload.
func TestCacheSizeCapStopsEncoderBeforeCommit(t *testing.T) {
	if maxCacheBytes != 512<<20 {
		t.Fatalf("cache size cap is %d bytes, expected 512MiB", maxCacheBytes)
	}
	var buf bytes.Buffer
	sink := &limitedWriter{w: &buf, limit: 8}
	if _, err := sink.Write([]byte("12345678")); err != nil {
		t.Fatalf("write exactly at the cap failed: %v", err)
	}
	if _, err := sink.Write([]byte("9")); !errors.Is(err, errCacheTooLarge) {
		t.Fatalf("write above the cap returned %v", err)
	}
	if buf.Len() != 8 {
		t.Fatalf("oversized write reached the cache file: %d bytes", buf.Len())
	}
}

func TestSaveRejectsInvalidIndexBeforeReplacingCache(t *testing.T) {
	root, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	cache := filepath.Join(root, "index.json")
	original := &Index{Entries: []Entry{{Path: filepath.Join(root, "old.txt"), Name: "old.txt"}}}
	if err := original.Save(cache); err != nil {
		t.Fatal(err)
	}
	before, err := os.ReadFile(cache)
	if err != nil {
		t.Fatal(err)
	}
	tooManyRoots := make([]string, maxRoots+1)
	for i := range tooManyRoots {
		tooManyRoots[i] = root
	}
	valid := Entry{Path: filepath.Join(root, "a.txt"), Name: "a.txt"}
	cases := map[string]*Index{
		"too-many-roots": {Entries: []Entry{valid}, Roots: tooManyRoots},
		"nul-root":       {Entries: []Entry{valid}, Roots: []string{root + "\x00bad"}},
		"nul-path":       {Entries: []Entry{{Path: root + "\x00bad", Name: "bad.txt"}}},
		"empty-name":     {Entries: []Entry{{Path: filepath.Join(root, "a.txt")}}},
		"negative-size":  {Entries: []Entry{{Path: filepath.Join(root, "a.txt"), Name: "a.txt", Size: -1}}},
	}
	for name, idx := range cases {
		if err := idx.Save(cache); err == nil {
			t.Errorf("Save accepted an invalid index (%s)", name)
			continue
		}
		after, err := os.ReadFile(cache)
		if err != nil {
			t.Fatal(err)
		}
		if !bytes.Equal(before, after) {
			t.Fatalf("rejected save (%s) changed the previous cache", name)
		}
	}
}

// cycleReader repeats a fixed pattern forever; io.LimitReader bounds it.
type cycleReader struct {
	pattern []byte
	pos     int
}

func (r *cycleReader) Read(p []byte) (int, error) {
	for i := range p {
		p[i] = r.pattern[r.pos]
		r.pos = (r.pos + 1) % len(r.pattern)
	}
	return len(p), nil
}

// The entry limit has no injection point either, so the stream is generated
// instead of being materialised: MaxEntries+1 minimal entries (about 48MB of
// JSON) must be refused before the whole array is accepted. Decoding that many
// entries costs about 950MB RSS because the accepted slice keeps growing, so the
// case is opt-in and never part of the default `go test ./...` run.
func TestLoadRejectsStreamOverEntryLimit(t *testing.T) {
	if testing.Short() || os.Getenv("OILFIND_HEAVY_TESTS") == "" {
		t.Skip("set OILFIND_HEAVY_TESTS=1 to stream MaxEntries+1 entries (about 950MB RSS)")
	}
	const entry = `{"path":"/a","name":"a"},`
	body := io.LimitReader(&cycleReader{pattern: []byte(entry)}, int64(MaxEntries+1)*int64(len(entry)))
	stream := io.MultiReader(
		strings.NewReader(`{"version":1,"entries":[`),
		body,
		strings.NewReader(`{"path":"/a","name":"a"}]}`),
	)
	idx, err := decodeCache(json.NewDecoder(stream))
	if err == nil || idx != nil {
		t.Fatalf("accepted a stream above the entry limit: index=%v err=%v", idx, err)
	}
	if !strings.Contains(err.Error(), "安全上限") {
		t.Fatalf("unexpected error for an oversized stream: %v", err)
	}
}
