package index

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	pinyin "github.com/mozillazg/go-pinyin"
)

const formatVersion = 1
const MaxEntries = 2_000_000
const maxCacheBytes = 512 << 20
const maxRoots = 64
const maxPathLen = 32767
const maxNameLen = 1024

var errCacheTooLarge = errors.New("索引缓存超过 512MB 上限，请缩小索引范围")

// Entry stores metadata only; no file contents are opened during indexing.
type Entry struct {
	Path       string `json:"path"`
	Name       string `json:"name"`
	Size       int64  `json:"size"`
	Modified   int64  `json:"modified"`
	IsDir      bool   `json:"isDir"`
	lowerName  string
	lowerPath  string
	fullPinyin string
	initials   string
}

type Index struct {
	Entries []Entry   `json:"entries"`
	Roots   []string  `json:"roots"`
	BuiltAt time.Time `json:"builtAt"`
}

type Progress struct {
	Visited, Skipped int
	Current          string
}

var excludedDirs = map[string]bool{
	"node_modules": true, ".git": true, ".svn": true, ".hg": true,
	".claude": true, ".codex": true, ".workbuddy": true, ".workbuddy-ai": true,
	".gemini": true, ".qwen": true, ".pnpm-store": true, ".build": true,
	".windsurf": true, ".freebuff": true, ".codegraph": true, ".reasonix": true,
	".agents": true, ".config": true, ".tmp": true,
	"appdata": true, "windows": true, "program files": true, "program files (x86)": true,
	"programdata": true, "$recycle.bin": true, "system volume information": true,
	"recovery": true, "config.msi": true,
}

// Scan only enumerates explicitly supplied, validated roots. Reparse points and
// symbolic links are never followed, including when passed as the root itself.
func Scan(ctx context.Context, roots []string, progress func(Progress)) (*Index, error) {
	normalized, err := normalizeRoots(roots)
	if err != nil {
		return nil, err
	}
	result := &Index{Entries: make([]Entry, 0, 1024), Roots: normalized, BuiltAt: time.Now()}
	stats := Progress{}
	lastReport := time.Now()
	report := func(force bool) {
		if progress != nil && (force || time.Since(lastReport) >= 150*time.Millisecond) {
			progress(stats)
			lastReport = time.Now()
		}
	}
	for _, root := range normalized {
		err = filepath.WalkDir(root, func(path string, d os.DirEntry, walkErr error) error {
			if err := ctx.Err(); err != nil {
				return err
			}
			stats.Current = path
			if walkErr != nil {
				stats.Skipped++
				report(false)
				if d != nil && d.IsDir() {
					return filepath.SkipDir
				}
				return nil
			}
			if isLinkOrReparse(path, d) {
				stats.Skipped++
				report(false)
				if d.IsDir() {
					return filepath.SkipDir
				}
				return nil
			}
			if d.IsDir() && (excludedDirs[strings.ToLower(d.Name())] || strings.EqualFold(d.Name(), "tmp")) {
				stats.Skipped++
				report(false)
				return filepath.SkipDir
			}
			if path == root {
				return nil
			}
			info, err := d.Info()
			if err != nil {
				stats.Skipped++
				report(false)
				return nil
			}
			if !info.IsDir() && !info.Mode().IsRegular() {
				stats.Skipped++
				return nil
			}
			entry := Entry{Path: path, Name: d.Name(), Size: info.Size(), Modified: info.ModTime().Unix(), IsDir: info.IsDir()}
			if entry.IsDir {
				entry.Size = 0
			}
			prepare(&entry)
			result.Entries = append(result.Entries, entry)
			stats.Visited++
			report(false)
			if len(result.Entries) >= MaxEntries {
				return fmt.Errorf("索引达到 %d 条安全上限，请缩小目录范围", MaxEntries)
			}
			return nil
		})
		if err != nil {
			return nil, err
		}
	}
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	sort.Slice(result.Entries, func(a, b int) bool { return result.Entries[a].lowerPath < result.Entries[b].lowerPath })
	report(true)
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	return result, nil
}

func normalizeRoots(roots []string) ([]string, error) {
	if len(roots) == 0 {
		return nil, errors.New("请先选择至少一个本机目录")
	}
	if len(roots) > maxRoots {
		return nil, errors.New("最多支持 64 个索引目录")
	}
	normalized := make([]string, 0, len(roots))
	for _, root := range roots {
		if strings.TrimSpace(root) == "" {
			return nil, errors.New("目录不能为空")
		}
		if !filepath.IsAbs(root) {
			return nil, errors.New("索引目录必须是绝对路径")
		}
		if strings.HasPrefix(root, `\\`) || strings.HasPrefix(root, "//") {
			return nil, errors.New("首版不支持网络目录或设备路径")
		}
		clean := filepath.Clean(root)
		if err := validateLocalRoot(clean); err != nil {
			return nil, err
		}
		info, err := os.Lstat(clean)
		if err != nil {
			return nil, fmt.Errorf("无法访问索引目录: %w", err)
		}
		if !info.IsDir() {
			return nil, errors.New("索引范围必须是目录")
		}
		// Reject links in every ancestor, not just the leaf selected by the user.
		for p := clean; ; p = filepath.Dir(p) {
			stat, err := os.Lstat(p)
			if err != nil {
				return nil, fmt.Errorf("无法核验目录: %w", err)
			}
			if isLinkOrReparse(p, fs.FileInfoToDirEntry(stat)) {
				return nil, errors.New("为避免越界，不能索引符号链接或目录联接")
			}
			if (p == clean && strings.EqualFold(filepath.Base(p), "tmp")) || excludedDirs[strings.ToLower(filepath.Base(p))] {
				return nil, errors.New("该目录属于系统、依赖或私人运行器目录，不在首版索引范围内")
			}
			if filepath.Dir(p) == p {
				break
			}
		}
		normalized = append(normalized, clean)
	}
	sort.Slice(normalized, func(a, b int) bool { return len(normalized[a]) < len(normalized[b]) })
	unique := make([]string, 0, len(normalized))
	for _, root := range normalized {
		covered := false
		for _, parent := range unique {
			if pathUnder(root, parent) {
				covered = true
				break
			}
		}
		if !covered {
			unique = append(unique, root)
		}
	}
	return unique, nil
}

func pathUnder(path, root string) bool {
	return platformPathEqual(path, root) || platformPathPrefix(path, strings.TrimRight(root, string(filepath.Separator))+string(filepath.Separator))
}

func prepare(e *Entry) {
	e.lowerName = strings.ToLower(e.Name)
	e.lowerPath = strings.ToLower(e.Path)
	if !strings.ContainsFunc(e.Name, func(r rune) bool { return r >= 0x3400 && r <= 0x9fff }) {
		return
	}
	args := pinyin.NewArgs()
	args.Style = pinyin.Normal
	args.Fallback = func(r rune, a pinyin.Args) []string { return []string{strings.ToLower(string(r))} }
	parts := pinyin.Pinyin(e.Name, args)
	var full, initial strings.Builder
	for _, part := range parts {
		if len(part) > 0 {
			full.WriteString(part[0])
			if part[0] != "" {
				initial.WriteByte(part[0][0])
			}
		}
	}
	e.fullPinyin = full.String()
	e.initials = initial.String()
}

// limitedWriter refuses to write more than limit bytes, so an oversized index
// is aborted before the temporary file can ever reach the cache size ceiling.
type limitedWriter struct {
	w     io.Writer
	limit int64
	n     int64
}

func (l *limitedWriter) Write(p []byte) (int, error) {
	if l.n+int64(len(p)) > l.limit {
		return 0, errCacheTooLarge
	}
	n, err := l.w.Write(p)
	l.n += int64(n)
	return n, err
}

// Save atomically replaces only the app's cache; failed writes leave the old
// index intact. Callers must not use a personal document as the cache filename.
func (idx *Index) Save(filename string) error {
	return idx.SaveContext(context.Background(), filename)
}

// SaveContext checks cancellation before committing a completed temp file.
// The encoded cache is size-capped, so Save can never produce a file that Load
// rejects as oversized.
func (idx *Index) SaveContext(ctx context.Context, filename string) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if len(idx.Entries) > MaxEntries {
		return errors.New("索引条目超出上限")
	}
	if len(idx.Roots) > maxRoots {
		return errors.New("索引目录数量超出上限")
	}
	for i := range idx.Entries {
		if err := validateEntry(&idx.Entries[i]); err != nil {
			return err
		}
	}
	for _, root := range idx.Roots {
		if err := validateRoot(root); err != nil {
			return err
		}
	}
	if err := os.MkdirAll(filepath.Dir(filename), 0700); err != nil {
		return err
	}
	temp, err := os.CreateTemp(filepath.Dir(filename), ".oilfind-index-*")
	if err != nil {
		return err
	}
	path := temp.Name()
	defer os.Remove(path)
	if err = temp.Chmod(0600); err != nil {
		temp.Close()
		return err
	}
	// Encode explicit fields: nil slices must still round-trip as empty arrays.
	entries := idx.Entries
	if entries == nil {
		entries = []Entry{}
	}
	roots := idx.Roots
	if roots == nil {
		roots = []string{}
	}
	payload := struct {
		Version int       `json:"version"`
		Roots   []string  `json:"roots"`
		BuiltAt time.Time `json:"builtAt"`
		Entries []Entry   `json:"entries"`
	}{formatVersion, roots, idx.BuiltAt, entries}
	sink := &limitedWriter{w: temp, limit: maxCacheBytes}
	err = json.NewEncoder(sink).Encode(payload)
	if err == nil {
		err = temp.Sync()
	}
	closeErr := temp.Close()
	if err == nil {
		err = closeErr
	}
	if err != nil {
		return err
	}
	if err := ctx.Err(); err != nil {
		return err
	}
	return replaceCache(path, filename)
}

func validateEntry(e *Entry) error {
	if e.Path == "" || e.Name == "" || e.Size < 0 ||
		len(e.Path) > maxPathLen || len(e.Name) > maxNameLen ||
		strings.ContainsRune(e.Path, 0) || strings.ContainsRune(e.Name, 0) {
		return errors.New("索引条目损坏")
	}
	return nil
}

func validateRoot(root string) error {
	if root == "" || len(root) > maxPathLen || strings.ContainsRune(root, 0) {
		return errors.New("索引目录损坏")
	}
	return nil
}

// Load streams the cache instead of decoding the whole entries array first:
// a hostile or corrupt file is rejected as soon as it exceeds MaxEntries, so
// it cannot force a multi-gigabyte allocation before the limit is checked.
func Load(filename string) (*Index, error) {
	info, err := os.Lstat(filename)
	if err != nil {
		return nil, err
	}
	if isLinkOrReparse(filename, fs.FileInfoToDirEntry(info)) {
		return nil, errors.New("索引缓存不能是链接或重解析点")
	}
	if !info.Mode().IsRegular() {
		return nil, errors.New("索引缓存必须是普通文件")
	}
	if info.Size() > maxCacheBytes {
		return nil, errors.New("索引缓存超过大小上限")
	}
	f, err := os.Open(filename)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	decoder := json.NewDecoder(io.LimitReader(f, maxCacheBytes+1))
	idx, err := decodeCache(decoder)
	if err != nil {
		return nil, err
	}
	if _, err := decoder.Token(); err != io.EOF {
		return nil, errors.New("索引包含多余数据")
	}
	return idx, nil
}

func decodeCache(decoder *json.Decoder) (*Index, error) {
	token, err := decoder.Token()
	if err != nil {
		return nil, err
	}
	if delim, ok := token.(json.Delim); !ok || delim != '{' {
		return nil, errors.New("索引格式无效")
	}
	idx := &Index{}
	var seenVersion, seenRoots, seenEntries bool
	for decoder.More() {
		keyToken, err := decoder.Token()
		if err != nil {
			return nil, err
		}
		key, ok := keyToken.(string)
		if !ok {
			return nil, errors.New("索引格式无效")
		}
		switch key {
		case "version":
			if seenVersion {
				return nil, errors.New("索引包含重复字段")
			}
			seenVersion = true
			var version int
			if err := decoder.Decode(&version); err != nil {
				return nil, err
			}
			if version != formatVersion {
				return nil, errors.New("索引版本不兼容，请重新索引")
			}
		case "roots":
			if seenRoots {
				return nil, errors.New("索引包含重复字段")
			}
			seenRoots = true
			roots, err := decodeRoots(decoder)
			if err != nil {
				return nil, err
			}
			idx.Roots = roots
		case "builtAt":
			if err := decoder.Decode(&idx.BuiltAt); err != nil {
				return nil, err
			}
		case "entries":
			if seenEntries {
				return nil, errors.New("索引包含重复字段")
			}
			seenEntries = true
			entries, err := decodeEntries(decoder)
			if err != nil {
				return nil, err
			}
			idx.Entries = entries
		default:
			if err := skipValue(decoder); err != nil {
				return nil, err
			}
		}
	}
	if _, err := decoder.Token(); err != nil {
		return nil, err
	}
	if !seenVersion {
		return nil, errors.New("索引版本不兼容，请重新索引")
	}
	return idx, nil
}

func decodeRoots(decoder *json.Decoder) ([]string, error) {
	token, err := decoder.Token()
	if err != nil {
		return nil, err
	}
	if token == nil {
		return nil, nil
	}
	if delim, ok := token.(json.Delim); !ok || delim != '[' {
		return nil, errors.New("索引格式无效")
	}
	roots := make([]string, 0, 8)
	for decoder.More() {
		if len(roots) >= maxRoots {
			return nil, errors.New("索引目录数量超出上限")
		}
		var root string
		if err := decoder.Decode(&root); err != nil {
			return nil, err
		}
		if err := validateRoot(root); err != nil {
			return nil, err
		}
		roots = append(roots, root)
	}
	if _, err := decoder.Token(); err != nil {
		return nil, err
	}
	return roots, nil
}

// decodeEntries consumes one entry at a time and stops at MaxEntries+1, so peak
// memory stays proportional to the accepted entries, never the declared count.
func decodeEntries(decoder *json.Decoder) ([]Entry, error) {
	token, err := decoder.Token()
	if err != nil {
		return nil, err
	}
	if token == nil {
		return nil, nil
	}
	if delim, ok := token.(json.Delim); !ok || delim != '[' {
		return nil, errors.New("索引格式无效")
	}
	entries := make([]Entry, 0, 1024)
	for decoder.More() {
		if len(entries) >= MaxEntries {
			return nil, fmt.Errorf("索引条目超过 %d 条安全上限", MaxEntries)
		}
		var entry Entry
		if err := decoder.Decode(&entry); err != nil {
			return nil, err
		}
		if err := validateEntry(&entry); err != nil {
			return nil, err
		}
		prepare(&entry)
		entries = append(entries, entry)
	}
	if _, err := decoder.Token(); err != nil {
		return nil, err
	}
	return entries, nil
}

func skipValue(decoder *json.Decoder) error {
	depth := 0
	for {
		token, err := decoder.Token()
		if err != nil {
			return err
		}
		if delim, ok := token.(json.Delim); ok {
			switch delim {
			case '{', '[':
				depth++
			case '}', ']':
				depth--
			}
		}
		if depth == 0 {
			return nil
		}
	}
}
