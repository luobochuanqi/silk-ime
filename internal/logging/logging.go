// Package logging 为 silkhost 提供结构化日志：logfmt 文本、按大小滚动、可选的用户文本脱敏。
//
// 输出格式与 silk-tsftip.dll 的日志一致（key=value 一行一条），两端日志可用同一套
// grep/筛选手段关联：host 侧以 conn 区分连接，握手日志记录对端 pid。
package logging

import (
	"fmt"
	"io"
	"log/slog"
	"os"
	"strings"
	"sync/atomic"
	"unicode/utf8"
)

// EnvLevel 覆盖配置文件中的日志级别，便于临时调试而不改配置；shim 读取同一变量。
const EnvLevel = "SILK_LOG"

// EnvText 为 "1" 时日志记录用户文本（preedit、候选、上屏）。默认只记长度：
// 运行日志不得包含用户输入（08 隐私约束），仅供本机排障时临时开启。
const EnvText = "SILK_LOG_TEXT"

var logText atomic.Bool

// Options 描述日志输出。
type Options struct {
	// Path 为日志文件路径；滚动备份为同目录下的 <name>.1.log … <name>.N.log。
	Path string
	// Level 为 debug/info/warn/error，空串取 info。环境变量 SILK_LOG 优先。
	Level string
	// MaxBytes 与 Backups 控制滚动：单文件上限与保留的备份数。
	MaxBytes int64
	Backups  int
	// Console 为 true 时同时写 stderr（前台调试运行）。
	Console bool
}

// Setup 按 opts 创建日志器并设为 slog 默认值。返回的 io.Closer 负责关闭日志文件，
// 调用方应在进程退出前调用。日志文件不可用时返回错误，此时调用方可继续以 stderr 运行。
func Setup(opts Options) (*slog.Logger, io.Closer, error) {
	level, err := ParseLevel(firstNonEmpty(os.Getenv(EnvLevel), opts.Level))
	if err != nil {
		return nil, nil, err
	}
	logText.Store(os.Getenv(EnvText) == "1")

	file, err := openRotating(opts.Path, opts.MaxBytes, opts.Backups)
	if err != nil {
		return nil, nil, err
	}
	var w io.Writer = file
	if opts.Console {
		// 文件在前：stderr 无效（无控制台）时 MultiWriter 在其后失败，不影响落盘。
		w = io.MultiWriter(file, os.Stderr)
	}
	logger := slog.New(slog.NewTextHandler(w, &slog.HandlerOptions{Level: level, ReplaceAttr: localTime}))
	slog.SetDefault(logger)
	return logger, file, nil
}

// localTime 把时间格式化为本地毫秒精度、不带时区，与 tsftip 日志一致，两份日志可按时间直接对齐。
func localTime(groups []string, a slog.Attr) slog.Attr {
	if a.Key == slog.TimeKey && len(groups) == 0 {
		return slog.String(slog.TimeKey, a.Value.Time().Local().Format("2006-01-02T15:04:05.000"))
	}
	return a
}

// ParseLevel 解析日志级别名（大小写不敏感）；空串为 info。
func ParseLevel(s string) (slog.Level, error) {
	switch strings.ToLower(strings.TrimSpace(s)) {
	case "", "info":
		return slog.LevelInfo, nil
	case "debug", "trace":
		return slog.LevelDebug, nil
	case "warn", "warning":
		return slog.LevelWarn, nil
	case "error":
		return slog.LevelError, nil
	}
	return 0, fmt.Errorf("未知日志级别 %q（可选 debug/info/warn/error）", s)
}

// Text 返回用户文本属性：开启 SILK_LOG_TEXT 时记录原文，否则只记字符数。
func Text(key, s string) slog.Attr {
	if logText.Load() {
		return slog.String(key, s)
	}
	return slog.Int(key+"_len", utf8.RuneCountInString(s))
}

func firstNonEmpty(values ...string) string {
	for _, v := range values {
		if v != "" {
			return v
		}
	}
	return ""
}
