package index

import (
	"errors"
	"fmt"
	"math"
	"path"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"time"
	"unicode"
)

type predicate func(*Entry) bool

type compiledQuery struct {
	groups    [][]predicate
	rankTerms []string
}

// Search is deterministic, immutable and safe to call concurrently on a
// completed index. Callers own the returned slice, not the underlying index.
func (idx *Index) Search(query string, limit int) ([]Entry, int, error) {
	return idx.SearchFiltered(query, "", limit)
}

// SearchFiltered applies the UI category outside the parsed OR groups.
func (idx *Index) SearchFiltered(query, kind string, limit int) ([]Entry, int, error) {
	var category predicate
	if kind != "" {
		term := "kind:" + kind
		if kind == "file" {
			term = "file:"
		}
		var err error
		category, _, err = termPredicate(term, time.Now())
		if err != nil {
			return nil, 0, err
		}
	}
	if len(query) > 4096 {
		return nil, 0, errors.New("查询最多支持 4096 字节")
	}
	compiled, err := compile(query, time.Now())
	if err != nil {
		return nil, 0, err
	}
	if limit < 0 {
		return nil, 0, errors.New("结果条数不能为负数")
	}
	if limit == 0 {
		limit = 2000
	}
	if limit > 10000 {
		limit = 10000
	}
	type ranked struct {
		entry Entry
		score int
	}
	candidates := make([]ranked, 0, min(limit, len(idx.Entries)))
	total := 0
	// Keep a bounded, ranked candidate set; avoid sorting millions of hits.
	better := func(a, b ranked) bool {
		if a.score != b.score {
			return a.score > b.score
		}
		return a.entry.lowerPath < b.entry.lowerPath
	}
	var heap []ranked
	for i := range idx.Entries {
		e := idx.Entries[i]
		if e.lowerName == "" {
			prepare(&e)
		}
		if category != nil && !category(&e) {
			continue
		}
		matches := len(compiled.groups) == 0
		for _, group := range compiled.groups {
			all := true
			for _, pred := range group {
				if !pred(&e) {
					all = false
					break
				}
			}
			if all {
				matches = true
				break
			}
		}
		if !matches {
			continue
		}
		total++
		score := 0
		for _, term := range compiled.rankTerms {
			switch {
			case e.lowerName == term:
				score += 100
			case strings.TrimSuffix(e.lowerName, path.Ext(e.lowerName)) == term:
				score += 90
			case strings.HasPrefix(e.lowerName, term):
				score += 60
			case strings.Contains(e.lowerName, term):
				score += 30
			case e.fullPinyin == term || e.initials == term:
				score += 25
			default:
				score += 10
			}
		}
		item := ranked{e, score}
		// A min-heap rooted at the worst retained entry.
		if len(heap) < limit {
			heap = append(heap, item)
			child := len(heap) - 1
			for child > 0 {
				parent := (child - 1) / 2
				if !better(heap[parent], heap[child]) {
					break
				}
				heap[parent], heap[child] = heap[child], heap[parent]
				child = parent
			}
		} else if better(item, heap[0]) {
			heap[0] = item
			parent := 0
			for {
				child := parent*2 + 1
				if child >= len(heap) {
					break
				}
				if child+1 < len(heap) && better(heap[child], heap[child+1]) {
					child++
				}
				if !better(heap[parent], heap[child]) {
					break
				}
				heap[parent], heap[child] = heap[child], heap[parent]
				parent = child
			}
		}
	}
	candidates = append(candidates, heap...)
	sort.Slice(candidates, func(i, j int) bool { return better(candidates[i], candidates[j]) })
	rows := make([]Entry, len(candidates))
	for i, item := range candidates {
		rows[i] = item.entry
	}
	return rows, total, nil
}

func compile(text string, now time.Time) (compiledQuery, error) {
	tokens, err := tokenize(text)
	if err != nil {
		return compiledQuery{}, err
	}
	if len(tokens) > 128 {
		return compiledQuery{}, errors.New("查询条件过多")
	}
	q := compiledQuery{}
	if len(tokens) == 0 {
		return q, nil
	}
	group := []predicate{}
	for _, token := range tokens {
		if token == "|" {
			if len(group) == 0 {
				return q, errors.New("OR 两侧不能为空")
			}
			q.groups = append(q.groups, group)
			group = nil
			continue
		}
		negated := strings.HasPrefix(token, "!")
		if negated {
			token = strings.TrimPrefix(token, "!")
			if token == "" {
				return q, errors.New("排除条件不能为空")
			}
		}
		pred, rank, err := termPredicate(token, now)
		if err != nil {
			return q, err
		}
		if negated {
			original := pred
			pred = func(e *Entry) bool { return !original(e) }
		} else if rank != "" {
			q.rankTerms = append(q.rankTerms, rank)
		}
		group = append(group, pred)
	}
	if len(group) == 0 {
		return q, errors.New("OR 两侧不能为空")
	}
	q.groups = append(q.groups, group)
	return q, nil
}

func tokenize(text string) ([]string, error) {
	var tokens []string
	var current strings.Builder
	var quote rune
	flush := func() {
		if current.Len() > 0 {
			tokens = append(tokens, current.String())
			current.Reset()
		}
	}
	for _, r := range text {
		if quote != 0 {
			if r == quote {
				quote = 0
				// An empty quoted token is a user error, not "match everything".
				if current.Len() == 0 {
					tokens = append(tokens, "")
				}
			} else {
				current.WriteRune(r)
			}
			continue
		}
		if r == '"' || r == '\'' {
			quote = r
			continue
		}
		if unicode.IsSpace(r) {
			flush()
			continue
		}
		// A bar in a regex stays part of the regular expression; surround OR
		// operators with spaces when combining a regex with other groups.
		if r == '|' && !strings.HasPrefix(current.String(), "regex:") && !strings.HasPrefix(current.String(), "!regex:") {
			flush()
			tokens = append(tokens, "|")
			continue
		}
		current.WriteRune(r)
	}
	if quote != 0 {
		return nil, errors.New("查询引号未闭合")
	}
	flush()
	return tokens, nil
}

var kinds = map[string][]string{
	"image":   {".png", ".jpg", ".jpeg", ".gif", ".bmp", ".webp", ".tif", ".tiff", ".svg", ".heic"},
	"doc":     {".pdf", ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx", ".txt", ".md", ".rtf", ".csv", ".odt", ".ods", ".odp", ".wps", ".et", ".dps", ".ofd"},
	"video":   {".mp4", ".mov", ".mkv", ".avi", ".wmv", ".webm", ".m4v"},
	"audio":   {".mp3", ".wav", ".flac", ".aac", ".m4a", ".ogg", ".wma"},
	"code":    {".go", ".py", ".js", ".ts", ".tsx", ".jsx", ".c", ".cpp", ".h", ".hpp", ".cs", ".java", ".rs", ".swift", ".html", ".css", ".json", ".yaml", ".yml", ".xml", ".sql", ".sh", ".ps1"},
	"archive": {".zip", ".7z", ".rar", ".gz", ".tar", ".bz2", ".xz"},
	"app":     {".exe", ".msi", ".msix", ".com"},
}

func termPredicate(token string, now time.Time) (predicate, string, error) {
	low := strings.ToLower(token)
	key, value, colon := strings.Cut(low, ":")
	if colon {
		switch key {
		case "regex":
			// Preserve source regex escapes and case; prepend insensitive mode.
			raw := token[len("regex:"):]
			if raw == "" {
				return nil, "", errors.New("正则表达式不能为空")
			}
			re, err := regexp.Compile("(?i)" + raw)
			if err != nil {
				return nil, "", fmt.Errorf("正则表达式无效: %w", err)
			}
			return func(e *Entry) bool { return re.MatchString(e.Name) }, "", nil
		case "ext":
			if value == "" {
				return nil, "", errors.New("扩展名不能为空")
			}
			extensions := map[string]bool{}
			for _, ext := range strings.Split(value, ";") {
				ext = strings.TrimPrefix(ext, ".")
				if ext == "" {
					return nil, "", errors.New("扩展名不能为空")
				}
				extensions["."+ext] = true
			}
			return func(e *Entry) bool { return !e.IsDir && extensions[strings.ToLower(filepath.Ext(e.Name))] }, "", nil
		case "kind":
			if value == "folder" {
				return func(e *Entry) bool { return e.IsDir }, "", nil
			}
			exts, ok := kinds[value]
			if !ok {
				return nil, "", errors.New("不支持的文件类型: " + value)
			}
			return func(e *Entry) bool {
				if e.IsDir {
					return false
				}
				ext := strings.ToLower(filepath.Ext(e.Name))
				for _, x := range exts {
					if x == ext {
						return true
					}
				}
				return false
			}, "", nil
		case "file", "folder":
			folder := key == "folder"
			return func(e *Entry) bool { return e.IsDir == folder && (value == "" || nameContains(e, value)) }, value, nil
		case "path":
			if value == "" {
				return nil, "", errors.New("路径条件不能为空")
			}
			value = strings.ReplaceAll(value, `\`, "/")
			return func(e *Entry) bool { return strings.Contains(strings.ReplaceAll(e.lowerPath, `\`, "/"), value) }, "", nil
		case "size":
			pred, err := sizePredicate(value)
			return pred, "", err
		case "dm":
			pred, err := datePredicate(value, now)
			return pred, "", err
		}
	}
	if strings.ContainsAny(low, "*?") {
		if _, err := path.Match(low, ""); err != nil {
			return nil, "", fmt.Errorf("通配符无效: %w", err)
		}
		return func(e *Entry) bool { ok, _ := path.Match(low, e.lowerName); return ok }, "", nil
	}
	if low == "" {
		return nil, "", errors.New("条件不能为空")
	}
	return func(e *Entry) bool { return nameContains(e, low) || strings.Contains(e.lowerPath, low) }, low, nil
}

func nameContains(e *Entry, term string) bool {
	return strings.Contains(e.lowerName, term) || strings.Contains(e.fullPinyin, term) || strings.Contains(e.initials, term)
}

func parseSize(text string) (int64, error) {
	units := []struct {
		suffix string
		factor float64
	}{{"tb", 1 << 40}, {"gb", 1 << 30}, {"mb", 1 << 20}, {"kb", 1 << 10}, {"b", 1}}
	factor := float64(1)
	for _, u := range units {
		if strings.HasSuffix(text, u.suffix) {
			text = strings.TrimSuffix(text, u.suffix)
			factor = u.factor
			break
		}
	}
	n, err := strconv.ParseFloat(text, 64)
	if err != nil || math.IsNaN(n) || math.IsInf(n, 0) || n < 0 || n >= float64(1<<63-1)/factor {
		return 0, errors.New("大小条件无效")
	}
	return int64(n * factor), nil
}
func sizePredicate(value string) (predicate, error) {
	if a, b, ok := strings.Cut(value, ".."); ok {
		lo, err := parseSize(a)
		if err != nil {
			return nil, err
		}
		hi, err := parseSize(b)
		if err != nil || hi < lo {
			return nil, errors.New("大小范围无效")
		}
		return func(e *Entry) bool { return !e.IsDir && e.Size >= lo && e.Size <= hi }, nil
	}
	op := "="
	for _, candidate := range []string{">=", "<=", ">", "<", "="} {
		if strings.HasPrefix(value, candidate) {
			op = candidate
			value = strings.TrimPrefix(value, candidate)
			break
		}
	}
	n, err := parseSize(value)
	if err != nil {
		return nil, err
	}
	return func(e *Entry) bool {
		if e.IsDir {
			return false
		}
		switch op {
		case ">":
			return e.Size > n
		case ">=":
			return e.Size >= n
		case "<":
			return e.Size < n
		case "<=":
			return e.Size <= n
		default:
			return e.Size == n
		}
	}, nil
}
func datePredicate(value string, now time.Time) (predicate, error) {
	today := time.Date(now.Year(), now.Month(), now.Day(), 0, 0, 0, 0, now.Location())
	var start, end time.Time
	switch value {
	case "today":
		start = today
		end = today.AddDate(0, 0, 1)
	case "yesterday":
		start = today.AddDate(0, 0, -1)
		end = today
	case "week":
		start = today.AddDate(0, 0, -6)
		end = today.AddDate(0, 0, 1)
	case "month":
		start = time.Date(now.Year(), now.Month(), 1, 0, 0, 0, 0, now.Location())
		end = start.AddDate(0, 1, 0)
	default:
		var err error
		if a, b, ok := strings.Cut(value, ".."); ok {
			start, err = time.ParseInLocation("2006-01-02", a, now.Location())
			if err != nil {
				return nil, errors.New("日期格式应为 YYYY-MM-DD")
			}
			end, err = time.ParseInLocation("2006-01-02", b, now.Location())
			if err != nil || end.Before(start) {
				return nil, errors.New("日期范围无效")
			}
			end = end.AddDate(0, 0, 1)
		} else {
			start, err = time.ParseInLocation("2006-01-02", value, now.Location())
			if err != nil {
				return nil, errors.New("日期格式应为 YYYY-MM-DD")
			}
			end = start.AddDate(0, 0, 1)
		}
	}
	return func(e *Entry) bool { return e.Modified >= start.Unix() && e.Modified < end.Unix() }, nil
}
