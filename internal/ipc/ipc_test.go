package ipc

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"math/rand/v2"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
)

const fixtureRoot = "../../protocol/fixtures"

func frameFixture(t *testing.T, body []byte) []byte {
	t.Helper()
	var buf bytes.Buffer
	if err := WriteFrame(&buf, body); err != nil {
		t.Fatalf("构造帧失败: %v", err)
	}
	return buf.Bytes()
}

// decodePayload 取载荷的通用表示，用于比较编解码前后的语义相等。
func decodePayload(t *testing.T, raw json.RawMessage) any {
	t.Helper()
	var value any
	if err := json.Unmarshal(raw, &value); err != nil {
		t.Fatalf("载荷解析失败: %v", err)
	}
	return value
}

func TestFixtureRoundTrip(t *testing.T) {
	entries, err := os.ReadDir(filepath.Join(fixtureRoot, "valid"))
	if err != nil {
		t.Fatalf("读取 fixtures 失败: %v", err)
	}
	seen := make(map[string]bool)
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".json") {
			continue
		}
		t.Run(entry.Name(), func(t *testing.T) {
			body, err := os.ReadFile(filepath.Join(fixtureRoot, "valid", entry.Name()))
			if err != nil {
				t.Fatalf("读取 fixture 失败: %v", err)
			}
			want, err := Decode(frameFixture(t, body))
			if err != nil {
				t.Fatalf("解码失败: %v", err)
			}
			seen[want.Type] = true

			frame, err := Encode(want)
			if err != nil {
				t.Fatalf("编码失败: %v", err)
			}
			got, err := Decode(frame)
			if err != nil {
				t.Fatalf("二次解码失败: %v", err)
			}
			if got.V != want.V || got.ID != want.ID || got.Type != want.Type || got.SID != want.SID {
				t.Fatalf("封套往返不一致: got %+v, want %+v", got, want)
			}
			if !reflect.DeepEqual(decodePayload(t, got.Payload), decodePayload(t, want.Payload)) {
				t.Fatalf("载荷往返不一致: got %s, want %s", got.Payload, want.Payload)
			}
		})
	}
	for _, typ := range []string{
		TypeHello, TypeWelcome, TypeFocusIn, TypeFocusOut, TypeKey, TypeAction,
		TypeUpdate, TypeCommit, TypeCaret, TypeShow, TypeHide, TypePing,
		TypePong, TypeError, TypeBye,
	} {
		if !seen[typ] {
			t.Errorf("缺少 %s 的合法样例", typ)
		}
	}
}

type malformedFixture struct {
	Reason   string `json:"reason"`
	FrameHex string `json:"frame_hex"`
}

func TestMalformedFixtures(t *testing.T) {
	entries, err := os.ReadDir(filepath.Join(fixtureRoot, "malformed"))
	if err != nil {
		t.Fatalf("读取 fixtures 失败: %v", err)
	}
	count := 0
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".json") {
			continue
		}
		count++
		t.Run(entry.Name(), func(t *testing.T) {
			raw, err := os.ReadFile(filepath.Join(fixtureRoot, "malformed", entry.Name()))
			if err != nil {
				t.Fatalf("读取 fixture 失败: %v", err)
			}
			var fixture malformedFixture
			if err := json.Unmarshal(raw, &fixture); err != nil {
				t.Fatalf("fixture 自身不是合法 JSON: %v", err)
			}
			frame, err := hex.DecodeString(fixture.FrameHex)
			if err != nil {
				t.Fatalf("frame_hex 非法: %v", err)
			}
			if _, err := Decode(frame); err == nil {
				t.Fatalf("期望解码失败（%s），实际成功", fixture.Reason)
			}
		})
	}
	if count < 5 {
		t.Fatalf("畸形样例不足 5 个，实际 %d", count)
	}
}

func TestDecodeRejectsOversizedLength(t *testing.T) {
	frame := []byte{0xff, 0xff, 0xff, 0xff}
	if _, err := Decode(frame); !errors.Is(err, ErrFrameTooLarge) {
		t.Fatalf("期望 ErrFrameTooLarge，实际 %v", err)
	}
	if _, err := ReadFrame(bytes.NewReader(frame)); !errors.Is(err, ErrFrameTooLarge) {
		t.Fatalf("ReadFrame 期望 ErrFrameTooLarge，实际 %v", err)
	}
	if err := WriteFrame(io.Discard, make([]byte, MaxFrameSize+1)); !errors.Is(err, ErrFrameTooLarge) {
		t.Fatalf("WriteFrame 期望 ErrFrameTooLarge，实际 %v", err)
	}
}

func TestReadFrameErrors(t *testing.T) {
	for name, data := range map[string][]byte{
		"空流":   nil,
		"帧头残缺": {0x01},
		"帧体残缺": {0x05, 0x00, 0x00, 0x00, '{'},
	} {
		if _, err := ReadFrame(bytes.NewReader(data)); err == nil {
			t.Errorf("%s: 期望错误，实际成功", name)
		}
	}
}

func TestEncodeRejectsInvalidEnvelope(t *testing.T) {
	cases := map[string]Envelope{
		"未知类型": {V: Version, ID: 1, Type: "bogus", Payload: json.RawMessage(`{}`)},
		"错误版本": {V: 2, ID: 1, Type: TypePing, Payload: json.RawMessage(`{"ts":1}`)},
		"缺会话":  {V: Version, ID: 1, Type: TypeHide, Payload: json.RawMessage(`{}`)},
		"缺字段":  {V: Version, ID: 1, Type: TypeKey, SID: "s", Payload: json.RawMessage(`{"code":1}`)},
		"字段类型": {V: Version, ID: 1, Type: TypePing, Payload: json.RawMessage(`{"ts":"now"}`)},
	}
	for name, env := range cases {
		if _, err := Encode(env); !errors.Is(err, ErrInvalidMessage) {
			t.Errorf("%s: 期望 ErrInvalidMessage，实际 %v", name, err)
		}
	}
}

func TestDecodeIgnoresUnknownFields(t *testing.T) {
	body := []byte(`{"v":1,"id":7,"type":"ping","payload":{"ts":12,"future":"x"},"extra":true}`)
	env, err := Decode(frameFixture(t, body))
	if err != nil {
		t.Fatalf("未知字段应被忽略: %v", err)
	}
	if env.ID != 7 || env.Type != TypePing {
		t.Fatalf("封套字段错误: %+v", env)
	}
}

// TestDecodeArbitraryBytes 用固定种子随机字节覆盖解码器的健壮性：任何输入
// 都只允许返回错误，不得 panic。
func TestDecodeArbitraryBytes(t *testing.T) {
	rng := rand.New(rand.NewPCG(2026, 1110))
	buf := make([]byte, 0, 256)
	for range 2000 {
		buf = buf[:rng.IntN(256)]
		for j := range buf {
			buf[j] = byte(rng.IntN(256))
		}
		Decode(buf)
	}
}

func FuzzDecode(f *testing.F) {
	for _, seed := range []string{
		``,
		"\x00\x00\x00\x00",
		"\xff\xff\xff\xff",
		`{"v":1,"id":1,"type":"ping","payload":{"ts":1}}`,
		"{\"v\":1,\"id\":1,\"type\":\"key\",\"sid\":\"s\",\"payload\":{\"code\":72,\"down\":true,\"mods\":0,\"ts\":1}}",
	} {
		f.Add([]byte(seed))
	}
	f.Fuzz(func(t *testing.T, frame []byte) {
		if env, err := Decode(frame); err == nil {
			if env.V != Version {
				t.Fatalf("成功解码出非法版本 %d", env.V)
			}
		}
	})
}
