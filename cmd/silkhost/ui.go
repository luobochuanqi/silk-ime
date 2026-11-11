package main

import (
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"syscall"
	"unsafe"

	"golang.org/x/sys/windows"
)

// candidateWindow 封装 silkui.dll 的 C ABI（契约见 06 设计）。
// 渲染失败仅影响候选窗，绝不阻塞输入：加载失败时宿主以无窗模式运行。
type candidateWindow struct {
	dll  windows.Handle
	show uintptr
	move uintptr
	hide uintptr
}

type silkCandidate struct {
	text    *byte
	comment *byte
}

type silkView struct {
	preedit    *byte
	candidates *silkCandidate
	count      int32
	pageIndex  int32
}

type silkRect struct{ x, y, w, h int32 }

// loadCandidateWindow 从宿主可执行文件旁加载渲染库。
func loadCandidateWindow() (*candidateWindow, error) {
	exe, err := os.Executable()
	if err != nil {
		return nil, fmt.Errorf("定位宿主可执行文件: %w", err)
	}
	dllPath := filepath.Join(filepath.Dir(exe), "silkui.dll")
	h, err := windows.LoadLibrary(dllPath)
	if err != nil {
		return nil, fmt.Errorf("加载 %s: %w", dllPath, err)
	}
	sym := func(name string) (uintptr, error) {
		return windows.GetProcAddress(h, name)
	}
	show, err := sym("silkui_show")
	if err == nil {
		var move, hide uintptr
		move, err = sym("silkui_move")
		if err == nil {
			hide, err = sym("silkui_hide")
		}
		if err == nil {
			return &candidateWindow{dll: h, show: show, move: move, hide: hide}, nil
		}
	}
	windows.FreeLibrary(h)
	return nil, fmt.Errorf("解析 silkui 导出: %w", err)
}

// cstr 返回 UTF-8 NUL 结尾的 C 字符串指针；内嵌 NUL 时截断（FFI 契约要求 C 字符串）。
func cstr(s string) *byte {
	if i := strings.IndexByte(s, 0); i >= 0 {
		s = s[:i]
	}
	p, _ := syscall.BytePtrFromString(s) // 截断后不含 NUL，不会失败
	return p
}

// render 显示/刷新候选窗；preedit 与候选内容在调用内拷贝进渲染库。
func (w *candidateWindow) render(preedit string, candidates []candidateMsg, pageIndex int, anchor silkRect, owner uintptr) {
	if w == nil {
		return
	}
	cands := make([]silkCandidate, len(candidates))
	for i, c := range candidates {
		cands[i] = silkCandidate{text: cstr(c.Text), comment: cstr(c.Comment)}
	}
	pre := cstr(preedit)
	view := silkView{preedit: pre, candidates: unsafe.SliceData(cands), count: int32(len(cands)), pageIndex: int32(pageIndex)}
	syscall.SyscallN(w.show, uintptr(unsafe.Pointer(&view)), uintptr(unsafe.Pointer(&anchor)), owner)
	runtime.KeepAlive(cands)
	runtime.KeepAlive(pre)
}

func (w *candidateWindow) moveAnchor(anchor silkRect) {
	if w == nil {
		return
	}
	syscall.SyscallN(w.move, uintptr(unsafe.Pointer(&anchor)))
}

func (w *candidateWindow) hideWindow() {
	if w == nil {
		return
	}
	syscall.SyscallN(w.hide)
}
