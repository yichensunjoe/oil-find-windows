package main

import (
	"fmt"
	"os"

	"oilfind-intranet/windows/internal/ui"
)

func main() {
	if err := ui.Run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
