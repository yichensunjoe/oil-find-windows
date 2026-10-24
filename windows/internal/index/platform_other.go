//go:build !windows

package index

import (
	"os"
	"strings"
)

func validateLocalRoot(root string) error             { return nil }
func isLinkOrReparse(path string, d os.DirEntry) bool { return d.Type()&os.ModeSymlink != 0 }
func platformPathEqual(a, b string) bool              { return a == b }
func platformPathPrefix(a, b string) bool             { return strings.HasPrefix(a, b) }
func replaceCache(source, dest string) error          { return os.Rename(source, dest) }
