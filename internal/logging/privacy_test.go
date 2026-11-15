package logging_test

import (
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"strings"
	"syscall"
	"testing"

	"github.com/luobochuanqi/silk-ime/internal/logging"
)

// 默认脱敏不得求值任意 Error/String 方法，更不能把其结果写入日志。
type privateFormatter struct{ called *bool }

func (v privateFormatter) String() string {
	*v.called = true
	return "private-formatter-content"
}

// 使用子进程覆盖启动配置，避免全局 logger、脱敏开关和环境污染其他测试。
func TestPrivacyOutput(t *testing.T) {
	if os.Getenv("SILK_TEST_LOGGING_CHILD") == "1" {
		privacyOutputChild(t)
		return
	}
	for _, mode := range []string{"default", "debug", "text", "error", "fallback"} {
		t.Run(mode, func(t *testing.T) {
			cmd := exec.Command(os.Args[0], "-test.run=^TestPrivacyOutput$")
			cmd.Env = append(os.Environ(), "SILK_TEST_LOGGING_CHILD=1", "SILK_TEST_LOGGING_MODE="+mode)
			if out, err := cmd.CombinedOutput(); err != nil {
				t.Fatalf("子进程失败: %v\n%s", err, out)
			}
		})
	}
}

func privacyOutputChild(t *testing.T) {
	mode := os.Getenv("SILK_TEST_LOGGING_MODE")
	t.Setenv(logging.EnvLevel, "info")
	t.Setenv(logging.EnvText, "0")
	if mode == "debug" {
		t.Setenv(logging.EnvLevel, "debug")
	}
	if mode == "text" || mode == "error" || mode == "fallback" {
		t.Setenv(logging.EnvText, "1")
	}
	if mode == "error" {
		t.Setenv(logging.EnvLevel, "error")
	}
	path := filepath.Join(t.TempDir(), "host.log")
	if mode == "fallback" {
		// 目录不能作为日志文件打开；捕获实际 fallback 的 stderr 输出。
		capture, err := os.Create(path)
		if err != nil {
			t.Fatal(err)
		}
		defer capture.Close()
		os.Stderr = capture
	}
	logPath := path
	if mode == "fallback" {
		logPath = filepath.Dir(path)
	}
	logger, closer, err := logging.Setup(logging.Options{Path: logPath, MaxBytes: 1 << 20, Backups: 1})
	if mode == "fallback" {
		if err == nil || closer != nil {
			t.Fatal("期望 stderr 降级")
		}
	} else if err != nil {
		t.Fatal(err)
	}
	const secret = "private-input-隐私"
	logger.Error("隐私测试", logging.Text("preedit", secret), logging.Identifier("sid", secret),
		logging.Sensitive("err", errors.New(secret)), logging.Sensitive("panic", secret))
	logger.Debug("调试隐私测试", logging.Text("commit", secret))
	logger.Error("系统错误", logging.Sensitive("os", fmt.Errorf("%s: %w", secret,
		&os.PathError{Op: secret, Path: secret, Err: syscall.Errno(5)})))
	logger.Error("语法错误", logging.Sensitive("syntax", fmt.Errorf("%s: %w", secret,
		&json.SyntaxError{Offset: 17})))
	logger.Error("类型错误", logging.Sensitive("mismatch", fmt.Errorf("%s: %w", secret,
		&json.UnmarshalTypeError{Value: secret, Type: reflect.TypeOf(0), Struct: secret, Field: secret, Offset: 23})))
	if os.Getenv(logging.EnvText) != "1" {
		called := false
		logger.Error("任意载荷", logging.Sensitive("panic", privateFormatter{called: &called}))
		if called {
			t.Fatal("默认脱敏调用了敏感格式化方法")
		}
	}
	if closer != nil {
		if err := closer.Close(); err != nil {
			t.Fatal(err)
		}
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	out := string(data)
	text := os.Getenv(logging.EnvText) == "1"
	if strings.Contains(out, secret) != text {
		t.Fatalf("原文开关不匹配: %s", out)
	}
	warnings := strings.Count(out, "SILK_LOG_TEXT=1")
	if (text && warnings != 1) || (!text && warnings != 0) {
		t.Fatalf("启动警告数量错误: %s", out)
	}
	if !text {
		for _, key := range []string{"preedit_len=", "sid_sha256=", "err_type=", "panic_type=",
			"os_errno=5", "syntax_kind=json_syntax", "syntax_offset=17", "mismatch_kind=json_type", "mismatch_offset=23"} {
			if !strings.Contains(out, key) {
				t.Errorf("缺少 %s: %s", key, out)
			}
		}
	}
	if slog.Default() != logger {
		t.Fatal("未安装默认日志器")
	}
}
