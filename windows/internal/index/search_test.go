package index_test

import (
	"fmt"
	"path/filepath"
	"reflect"
	"sort"
	"testing"
	"time"

	index "oilfind-intranet/windows/internal/index"
)

func searchFixture(t *testing.T) *index.Index {
	t.Helper()
	root := testTempDir(t)
	now := time.Now().Unix()
	old := time.Now().AddDate(0, 0, -40).Unix()
	const mib = int64(1024 * 1024)
	entries := []index.Entry{
		{Path: filepath.Join(root, "reports", "Report Final.PDF"), Name: "Report Final.PDF", Size: 12 * mib, Modified: now},
		{Path: filepath.Join(root, "reports", "report draft.docx"), Name: "report draft.docx", Size: 3 * mib, Modified: old},
		{Path: filepath.Join(root, "notes", "report notes.txt"), Name: "report notes.txt", Size: mib / 2, Modified: old},
		{Path: filepath.Join(root, "images", "IMG_001.PNG"), Name: "IMG_001.PNG", Size: 2 * mib, Modified: now},
		{Path: filepath.Join(root, "images", "photo.jpg"), Name: "photo.jpg", Size: mib / 4, Modified: old},
		{Path: filepath.Join(root, "docs", "会议纪要.docx"), Name: "会议纪要.docx", Size: mib, Modified: now},
		{Path: filepath.Join(root, "docs", "预算报告.pdf"), Name: "预算报告.pdf", Size: 5 * mib, Modified: old},
		{Path: filepath.Join(root, "source", "main.go"), Name: "main.go", Size: 2048, Modified: old},
		{Path: filepath.Join(root, "reports"), Name: "reports", IsDir: true, Modified: now},
		{Path: filepath.Join(root, "archive", "report old.pdf"), Name: "report old.pdf", Size: 20 * mib, Modified: old},
	}
	return &index.Index{Entries: entries}
}

func assertSearchNames(t *testing.T, idx *index.Index, query string, want []string) {
	t.Helper()
	got, total, err := idx.Search(query, 100)
	if err != nil {
		t.Fatalf("Search(%q): %v", query, err)
	}
	if total != len(want) || len(got) != len(want) {
		t.Fatalf("Search(%q): total=%d results=%d, want %d; results=%v", query, total, len(got), len(want), got)
	}
	names := make([]string, len(got))
	for i, entry := range got {
		names[i] = entry.Name
	}
	want = append([]string(nil), want...)
	sort.Strings(names)
	sort.Strings(want)
	if len(want) == 0 {
		return
	}
	if !reflect.DeepEqual(names, want) {
		t.Fatalf("Search(%q): names=%v, want %v", query, names, want)
	}
}

func TestSearchTextAndBoolean(t *testing.T) {
	idx := searchFixture(t)
	cases := []struct {
		name  string
		query string
		want  []string
	}{
		{"case-insensitive", "rEpOrT", []string{"Report Final.PDF", "report draft.docx", "report notes.txt", "reports", "report old.pdf"}},
		{"chinese-substring", "会议", []string{"会议纪要.docx"}},
		{"full-pinyin", "huiyijiyao", []string{"会议纪要.docx"}},
		{"pinyin-initials", "hyjy", []string{"会议纪要.docx"}},
		{"pinyin-case-insensitive", "HUIYI", []string{"会议纪要.docx"}},
		{"and", "report final", []string{"Report Final.PDF"}},
		{"and-unmatched", "report photo", nil},
		{"or", "会议 | photo", []string{"会议纪要.docx", "photo.jpg"}},
		{"or-with-and", "report final | photo", []string{"Report Final.PDF", "photo.jpg"}},
		{"exclude-name", "report !draft", []string{"Report Final.PDF", "report notes.txt", "reports", "report old.pdf"}},
		{"exclude-path", "report !archive", []string{"Report Final.PDF", "report draft.docx", "report notes.txt", "reports"}},
		{"or-with-exclusion", "report !archive | photo", []string{"Report Final.PDF", "report draft.docx", "report notes.txt", "reports", "photo.jpg"}},
		{"path-substring", "path:archive", []string{"report old.pdf"}},
		{"missing", "unfindable-needle", nil},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			assertSearchNames(t, idx, tc.query, tc.want)
		})
	}
}

func TestSearchFilters(t *testing.T) {
	idx := searchFixture(t)
	cases := []struct {
		name  string
		query string
		want  []string
	}{
		{"extension-case-insensitive", "ext:PDF", []string{"Report Final.PDF", "预算报告.pdf", "report old.pdf"}},
		{"multiple-extensions", "ext:pdf;docx", []string{"Report Final.PDF", "report draft.docx", "会议纪要.docx", "预算报告.pdf", "report old.pdf"}},
		{"image-kind", "kind:image", []string{"IMG_001.PNG", "photo.jpg"}},
		{"folder-kind", "kind:folder", []string{"reports"}},
		{"code-kind", "kind:code", []string{"main.go"}},
		{"file-name", "file:report", []string{"Report Final.PDF", "report draft.docx", "report notes.txt", "report old.pdf"}},
		{"folder-name", "folder:report", []string{"reports"}},
		{"size-greater", "size:>10mb", []string{"Report Final.PDF", "report old.pdf"}},
		{"size-range-inclusive", "size:1mb..5mb", []string{"report draft.docx", "IMG_001.PNG", "会议纪要.docx", "预算报告.pdf"}},
		{"date-today", "dm:today", []string{"Report Final.PDF", "IMG_001.PNG", "会议纪要.docx", "reports"}},
		{"date-week", "dm:week", []string{"Report Final.PDF", "IMG_001.PNG", "会议纪要.docx", "reports"}},
		{"date-exact", "dm:" + time.Now().Format("2006-01-02"), []string{"Report Final.PDF", "IMG_001.PNG", "会议纪要.docx", "reports"}},
		{"glob", "*.png", []string{"IMG_001.PNG"}},
		{"regex", "regex:^IMG_", []string{"IMG_001.PNG"}},
		{"combined-filters", "report ext:pdf size:>10mb !archive", []string{"Report Final.PDF"}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			assertSearchNames(t, idx, tc.query, tc.want)
		})
	}
}

func TestSearchDocumentKind(t *testing.T) {
	root := testTempDir(t)
	idx := &index.Index{Entries: []index.Entry{
		{Path: filepath.Join(root, "a.pdf"), Name: "a.pdf"},
		{Path: filepath.Join(root, "b.docx"), Name: "b.docx"},
		{Path: filepath.Join(root, "c.png"), Name: "c.png"},
		{Path: filepath.Join(root, "d.go"), Name: "d.go"},
		{Path: filepath.Join(root, "folder"), Name: "folder", IsDir: true},
	}}
	assertSearchNames(t, idx, "kind:doc", []string{"a.pdf", "b.docx"})
}

func TestSearchInvalidSyntax(t *testing.T) {
	idx := searchFixture(t)
	for _, query := range []string{"regex:[", "regex:(", "size:>not-a-size", "size:5mb..1mb", "dm:not-a-date", "report |", "| report"} {
		t.Run(query, func(t *testing.T) {
			if _, _, err := idx.Search(query, 20); err == nil {
				t.Fatalf("Search(%q) accepted invalid syntax", query)
			}
		})
	}
}

func TestSearchRankingAndDeterministicPathOrder(t *testing.T) {
	root := testTempDir(t)
	idx := &index.Index{Entries: []index.Entry{
		{Path: filepath.Join(root, "z", "xreport"), Name: "xreport"},
		{Path: filepath.Join(root, "b", "report"), Name: "report"},
		{Path: filepath.Join(root, "z", "report-extra"), Name: "report-extra"},
		{Path: filepath.Join(root, "a", "report"), Name: "report"},
	}}
	want := []string{idx.Entries[3].Path, idx.Entries[1].Path, idx.Entries[2].Path, idx.Entries[0].Path}
	for iteration := 0; iteration < 3; iteration++ {
		got, total, err := idx.Search("report", 100)
		if err != nil || total != len(want) || len(got) != len(want) {
			t.Fatalf("Search: results=%v total=%d err=%v", got, total, err)
		}
		paths := make([]string, len(got))
		for i, entry := range got {
			paths[i] = entry.Path
		}
		if !reflect.DeepEqual(paths, want) {
			t.Fatalf("ranked paths=%v, want %v", paths, want)
		}
	}
}

func TestSearchLimitPreservesTotal(t *testing.T) {
	root := testTempDir(t)
	const count = 10000
	idx := &index.Index{Entries: make([]index.Entry, count)}
	for i := range idx.Entries {
		name := fmt.Sprintf("record-%05d.txt", count-i-1)
		idx.Entries[i] = index.Entry{Path: filepath.Join(root, name), Name: name, Size: int64(i)}
	}
	for _, limit := range []int{1, 7, 100, count + 1} {
		got, total, err := idx.Search("record", limit)
		if err != nil {
			t.Fatalf("limit=%d: %v", limit, err)
		}
		wantLen := limit
		if wantLen > count {
			wantLen = count
		}
		if total != count || len(got) != wantLen {
			t.Fatalf("limit=%d: total=%d len=%d, want total=%d len=%d", limit, total, len(got), count, wantLen)
		}
		for i, entry := range got {
			wantName := fmt.Sprintf("record-%05d.txt", i)
			if entry.Name != wantName {
				t.Fatalf("limit=%d position=%d: name=%q, want %q", limit, i, entry.Name, wantName)
			}
		}
	}
}

func TestSearchFilteredCategoryCannotBeBypassedByOR(t *testing.T) {
	idx := searchFixture(t)
	cases := []struct {
		name  string
		query string
		kind  string
		want  []string
	}{
		{"image-or", "report | photo", "image", []string{"photo.jpg"}},
		{"doc-or", "report | photo", "doc", []string{"Report Final.PDF", "report draft.docx", "report notes.txt", "report old.pdf"}},
		{"folder-or", "report | photo", "folder", []string{"reports"}},
		{"code-or", "report | main", "code", []string{"main.go"}},
		{"file-or", "report | photo", "file", []string{"Report Final.PDF", "report draft.docx", "report notes.txt", "report old.pdf", "photo.jpg"}},
		{"conflicting-query-kinds", "kind:folder | kind:image", "doc", nil},
		{"matching-query-kind", "kind:folder | kind:image", "image", []string{"IMG_001.PNG", "photo.jpg"}},
		{"exclude-or", "!photo | report", "image", []string{"IMG_001.PNG"}},
		{"empty-query", "", "image", []string{"IMG_001.PNG", "photo.jpg"}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, total, err := idx.SearchFiltered(tc.query, tc.kind, 100)
			if err != nil {
				t.Fatalf("SearchFiltered(%q, %q): %v", tc.query, tc.kind, err)
			}
			if total != len(tc.want) || len(got) != len(tc.want) {
				t.Fatalf("SearchFiltered(%q, %q): total=%d len=%d, want %d; results=%v", tc.query, tc.kind, total, len(got), len(tc.want), got)
			}
			names := make([]string, len(got))
			for i, entry := range got {
				names[i] = entry.Name
			}
			want := append([]string(nil), tc.want...)
			sort.Strings(names)
			sort.Strings(want)
			if len(want) > 0 && !reflect.DeepEqual(names, want) {
				t.Fatalf("filtered names=%v, want %v", names, want)
			}
		})
	}
}

func TestSearchFilteredLimitAndValidation(t *testing.T) {
	idx := searchFixture(t)
	got, total, err := idx.SearchFiltered("report | photo | IMG_", "image", 1)
	if err != nil || total != 2 || len(got) != 1 {
		t.Fatalf("filtered limit: total=%d len=%d err=%v", total, len(got), err)
	}
	if got[0].IsDir || (got[0].Name != "photo.jpg" && got[0].Name != "IMG_001.PNG") {
		t.Fatalf("non-image escaped category filter: %+v", got[0])
	}
	plain, plainTotal, err := idx.Search("report | photo", 100)
	if err != nil {
		t.Fatal(err)
	}
	filtered, filteredTotal, err := idx.SearchFiltered("report | photo", "", 100)
	if err != nil || filteredTotal != plainTotal {
		t.Fatalf("empty category: total=%d, want %d; err=%v", filteredTotal, plainTotal, err)
	}
	assertEntryMetadata(t, filtered, plain)
	if _, _, err := idx.SearchFiltered("report | photo", "unknown-category", 100); err == nil {
		t.Fatal("SearchFiltered accepted an unknown category")
	}
	if _, _, err := idx.SearchFiltered("regex:[", "image", 100); err == nil {
		t.Fatal("SearchFiltered suppressed an invalid query")
	}
}

func TestSearchDoesNotReadFileContents(t *testing.T) {
	root := testTempDir(t)
	idx := &index.Index{Entries: []index.Entry{{Path: filepath.Join(root, "not-created.txt"), Name: "not-created.txt"}}}
	assertSearchNames(t, idx, "not-created", []string{"not-created.txt"})
}
