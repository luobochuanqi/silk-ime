package main

import (
	"bytes"
	"encoding/binary"
	"encoding/json"
	"errors"
	"io"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/luobochuanqi/silk-ime/internal/engine"
	"github.com/luobochuanqi/silk-ime/internal/ipc"
	"github.com/luobochuanqi/silk-ime/internal/logging"
)

// 消费者回归走实际 conn 日志出口；子进程隔离 Setup 的全局状态。
func TestConnectionLogPrivacy(t *testing.T) {
	if os.Getenv("SILK_TEST_CONN_PRIVACY_CHILD") == "1" {
		connectionLogPrivacyChild(t)
		return
	}
	for _, mode := range []string{"default", "debug", "text"} {
		t.Run(mode, func(t *testing.T) {
			cmd := exec.Command(os.Args[0], "-test.run=^TestConnectionLogPrivacy$")
			cmd.Env = append(os.Environ(), "SILK_TEST_CONN_PRIVACY_CHILD=1", "SILK_TEST_CONN_PRIVACY_MODE="+mode)
			if out, err := cmd.CombinedOutput(); err != nil {
				t.Fatalf("子进程失败: %v\n%s", err, out)
			}
		})
	}
}

const logPrivacySecret = "private-input-隐私"

// 模拟不可信传输错误和 panic，写入不阻塞，便于检查完整收尾日志。
type privacyConn struct {
	net.Conn
	panicRead bool
	reader    io.Reader
}

func (c privacyConn) Read(p []byte) (int, error) {
	if c.panicRead {
		panic(logPrivacySecret)
	}
	if c.reader != nil {
		return c.reader.Read(p)
	}
	return 0, errors.New(logPrivacySecret)
}

func (privacyConn) Write(p []byte) (int, error) { return 0, errors.New(logPrivacySecret) }
func (privacyConn) Close() error                { return nil }

func connectionLogPrivacyChild(t *testing.T) {
	mode := os.Getenv("SILK_TEST_CONN_PRIVACY_MODE")
	t.Setenv(logging.EnvLevel, "info")
	t.Setenv(logging.EnvText, "0")
	if mode == "debug" || mode == "text" {
		t.Setenv(logging.EnvLevel, "debug")
	}
	if mode == "text" {
		t.Setenv(logging.EnvText, "1")
	}
	path := filepath.Join(t.TempDir(), "host.log")
	_, closer, err := logging.Setup(logging.Options{Path: path, MaxBytes: 1 << 20, Backups: 1})
	if err != nil {
		t.Fatal(err)
	}
	mgr := newTestManager(t)
	c := newConn(7, privacyConn{}, mgr, &presenter{}, 5)
	env := ipc.Envelope{V: ipc.Version, ID: 42, Type: logPrivacySecret, SID: logPrivacySecret}
	c.dispatch(env)
	payload := func(v any) json.RawMessage {
		raw, err := json.Marshal(v)
		if err != nil {
			t.Fatal(err)
		}
		return raw
	}
	c.hello(ipc.Envelope{Payload: payload(ipc.Hello{V: ipc.Version, Build: logPrivacySecret})})
	env.Type, env.Payload = ipc.TypeFocusIn, payload(ipc.FocusIn{App: logPrivacySecret})
	c.dispatch(env)
	env.Type, env.Payload = ipc.TypeAction, payload(ipc.Action{Kind: logPrivacySecret})
	c.dispatch(env)
	// 非法按键载荷走解码错误日志出口，不允许错误链泄漏输入。
	env.Type, env.Payload = ipc.TypeKey, payload(map[string]any{"code": logPrivacySecret})
	c.dispatch(env)
	env.Type = ipc.TypeCaret
	c.caret(env)
	c.trace(env, engine.Response{Preedit: logPrivacySecret, Commit: logPrivacySecret}, time.Millisecond)
	c.trace(env, engine.Response{Preedit: logPrivacySecret, Commit: logPrivacySecret}, slowRequest)
	c.run()
	newConn(8, privacyConn{panicRead: true}, mgr, &presenter{}, 5).run()
	badJSON := []byte(`{"v":1,"id":1,"type":"` + logPrivacySecret + `","payload":{}}`)
	frame := make([]byte, 4+len(badJSON))
	binary.LittleEndian.PutUint32(frame, uint32(len(badJSON)))
	copy(frame[4:], badJSON)
	newConn(9, privacyConn{reader: bytes.NewReader(frame)}, mgr, &presenter{}, 5).run()
	if err := closer.Close(); err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	out := string(data)
	if strings.Contains(out, logPrivacySecret) != (mode == "text") {
		t.Fatalf("消费者泄漏或原文开关无效: %s", out)
	}
	for _, field := range []string{"conn=7", "id=42", "type=action", "连接处理崩溃", "读取失败", "解码失败"} {
		if !strings.Contains(out, field) {
			t.Errorf("缺少关联/错误信息 %q: %s", field, out)
		}
	}
	if mode != "text" {
		for _, field := range []string{"sid_sha256=", "preedit_len=", "commit_len=", "err_type=", "panic_type="} {
			if !strings.Contains(out, field) {
				t.Errorf("缺少脱敏字段 %q: %s", field, out)
			}
		}
	}
}
