//go:build !windows

package ui

import "errors"

func Run() error {
	return errors.New("Oil Find for Windows 的原生界面仅支持 Windows")
}
