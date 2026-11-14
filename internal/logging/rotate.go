package logging

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
)

// rotatingFile 是按大小滚动的追加写文件：写入将超过上限时把 name.log 依次后移为
// name.1.log … name.N.log，再重新打开 name.log。并发安全。
type rotatingFile struct {
	mu       sync.Mutex
	path     string
	maxBytes int64
	backups  int
	f        *os.File
	size     int64
}

func openRotating(path string, maxBytes int64, backups int) (*rotatingFile, error) {
	if maxBytes <= 0 {
		maxBytes = 2 << 20
	}
	if backups < 0 {
		backups = 0
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return nil, fmt.Errorf("创建日志目录: %w", err)
	}
	w := &rotatingFile{path: path, maxBytes: maxBytes, backups: backups}
	if err := w.open(); err != nil {
		return nil, err
	}
	return w, nil
}

func (w *rotatingFile) open() error {
	f, err := os.OpenFile(w.path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return fmt.Errorf("打开日志文件: %w", err)
	}
	info, err := f.Stat()
	if err != nil {
		f.Close()
		return fmt.Errorf("读取日志文件信息: %w", err)
	}
	w.f, w.size = f, info.Size()
	return nil
}

// Write 写入一条日志。滚动失败不丢日志：继续写当前文件，下次写入再试。
func (w *rotatingFile) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.f == nil {
		return 0, os.ErrClosed
	}
	if w.size > 0 && w.size+int64(len(p)) > w.maxBytes {
		w.rotate()
	}
	n, err := w.f.Write(p)
	w.size += int64(n)
	return n, err
}

func (w *rotatingFile) rotate() {
	if err := w.f.Close(); err != nil {
		return
	}
	for i := w.backups - 1; i >= 1; i-- {
		_ = os.Rename(w.backupName(i), w.backupName(i+1))
	}
	if w.backups > 0 {
		_ = os.Rename(w.path, w.backupName(1))
	} else {
		_ = os.Truncate(w.path, 0)
	}
	if err := w.open(); err != nil {
		// 重开失败时保持 f 为空：后续写入返回错误而不是写已关闭的句柄。
		w.f = nil
	}
}

func (w *rotatingFile) backupName(i int) string {
	ext := filepath.Ext(w.path)
	return fmt.Sprintf("%s.%d%s", strings.TrimSuffix(w.path, ext), i, ext)
}

// Close 关闭日志文件；之后的写入返回 os.ErrClosed。
func (w *rotatingFile) Close() error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.f == nil {
		return nil
	}
	err := w.f.Close()
	w.f = nil
	return err
}
