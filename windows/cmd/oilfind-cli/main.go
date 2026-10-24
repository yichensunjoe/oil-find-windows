package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"strings"

	"oilfind/internal/index"
)

type rootsFlag []string

func (r *rootsFlag) String() string     { return strings.Join(*r, ";") }
func (r *rootsFlag) Set(s string) error { *r = append(*r, s); return nil }

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
func run() error {
	var roots rootsFlag
	flag.Var(&roots, "root", "Explicit absolute local directory to index (repeatable)")
	scan := flag.Bool("scan", false, "Build metadata index for explicitly selected roots")
	query := flag.String("query", "", "Search an existing cache without scanning files")
	output := flag.String("cache", "", "App cache filename (default: application config directory)")
	limit := flag.Int("limit", 100, "Maximum returned entries")
	flag.Parse()
	if *output == "" {
		base, err := os.UserConfigDir()
		if err != nil {
			return err
		}
		*output = filepath.Join(base, "OilFind", "index.json")
	}
	var idx *index.Index
	var err error
	if *scan {
		if len(roots) == 0 {
			return fmt.Errorf("扫描必须显式指定 -root，默认不会扫描任何个人目录")
		}
		ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt)
		defer cancel()
		idx, err = index.Scan(ctx, roots, nil)
		if err != nil {
			return err
		}
		// A canceled scan must never overwrite the previous cache.
		if err = ctx.Err(); err != nil {
			return err
		}
		if err = idx.SaveContext(ctx, *output); err != nil {
			return err
		}
		if err = ctx.Err(); err != nil {
			return err
		}
	} else {
		if len(roots) > 0 {
			return fmt.Errorf("仅有 -root 不会启动扫描，请明确添加 -scan")
		}
		idx, err = index.Load(*output)
		if err != nil {
			return err
		}
	}
	rows, total, err := idx.Search(*query, *limit)
	if err != nil {
		return err
	}
	return json.NewEncoder(os.Stdout).Encode(struct {
		Total   int           `json:"total"`
		Entries []index.Entry `json:"entries"`
	}{total, rows})
}
