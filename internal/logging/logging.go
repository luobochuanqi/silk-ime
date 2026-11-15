// Package logging 为 silkhost 提供结构化日志：logfmt 文本、按大小滚动、可选的用户文本脱敏。
//
// 输出格式与 silk-tsftip.dll 的日志一致（key=value 一行一条），两端日志可用同一套
// grep/筛选手段关联：host 侧以 conn 区分连接，握手日志记录对端 pid。
package logging

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"strings"
	"sync/atomic"
	"syscall"
	"time"
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

// Setup 创建日志器并设为 slog 默认值；返回的 io.Closer 由调用方在退出时关闭。
// 文件或级别配置不可用时仍安装 stderr 日志器并返回错误，保证隐私策略与告警生效。
func Setup(opts Options) (*slog.Logger, io.Closer, error) {
	logText.Store(os.Getenv(EnvText) == "1")
	level, err := ParseLevel(firstNonEmpty(os.Getenv(EnvLevel), opts.Level))
	if err != nil {
		level = slog.LevelInfo
	}
	var closer io.Closer
	var w io.Writer = os.Stderr
	if err == nil {
		file, openErr := openRotating(opts.Path, opts.MaxBytes, opts.Backups)
		err = openErr
		if err == nil {
			closer, w = file, file
			if opts.Console {
				// 文件在前：stderr 无效时不影响落盘。
				w = io.MultiWriter(file, os.Stderr)
			}
		}
	}
	logger := slog.New(slog.NewTextHandler(w, &slog.HandlerOptions{Level: level, ReplaceAttr: localTime}))
	slog.SetDefault(logger)
	if logText.Load() {
		// 直接交给 handler：即使仅记录 error，也不能过滤掉隐私风险告警。
		_ = logger.Handler().Handle(context.Background(), slog.NewRecord(time.Now(), slog.LevelWarn,
			"SILK_LOG_TEXT=1：日志将包含用户输入及敏感内容，仅限临时本机排障", 0))
	}
	return logger, closer, err
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

// Identifier 默认只保留摘要，便于关联不可信的会话标识而不记录原文。
func Identifier(key, s string) slog.Attr {
	if logText.Load() {
		return Text(key, s)
	}
	sum := sha256.Sum256([]byte(s))
	return slog.String(key+"_sha256", fmt.Sprintf("%x", sum[:16]))
}

// Sensitive 处理可能把输入嵌入 Error/String 方法的任意值；默认不调用这些方法。
func Sensitive(key string, value any) slog.Attr {
	if logText.Load() {
		return Text(key, fmt.Sprint(value))
	}
	typ := slog.String(key+"_type", fmt.Sprintf("%T", value))
	if err, ok := value.(error); ok {
		// 仅提取类型化数值，绝不调用 Error/String 或记录错误携带的路径、字段、原文。
		var errno syscall.Errno
		var syntax *json.SyntaxError
		var mismatch *json.UnmarshalTypeError
		switch {
		case errors.As(err, &errno):
			return slog.Group("", typ, slog.Uint64(key+"_errno", uint64(errno)))
		case errors.As(err, &syntax) && syntax != nil:
			return slog.Group("", typ, slog.String(key+"_kind", "json_syntax"), slog.Int64(key+"_offset", syntax.Offset))
		case errors.As(err, &mismatch) && mismatch != nil:
			return slog.Group("", typ, slog.String(key+"_kind", "json_type"), slog.Int64(key+"_offset", mismatch.Offset))
		}
	}
	return typ
}

func firstNonEmpty(values ...string) string {
	for _, v := range values {
		if v != "" {
			return v
		}
	}
	return ""
}
