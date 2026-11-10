package engine

import (
	"os"
	"sync"
	"testing"

	"github.com/luobochuanqi/silk-ime/internal/dict"
)

var (
	dictOnce sync.Once
	dictBin  []byte
	dictErr  error
)

// testDict 从仓库内的词库源文件构建二进制，测试与发布词库共用同一份数据；
// 构建结果在进程内缓存，避免每个用例重复解析。
func testDict(t testing.TB) []byte {
	t.Helper()
	dictOnce.Do(func() {
		var data []byte
		data, dictErr = os.ReadFile("../../dict/source.tsv")
		if dictErr != nil {
			return
		}
		var src []dict.Source
		src, dictErr = dict.ParseSource(data)
		if dictErr != nil {
			return
		}
		dictBin, dictErr = dict.Build(src)
	})
	if dictErr != nil {
		t.Fatalf("准备测试词库: %v", dictErr)
	}
	return dictBin
}

func newTestEngine(t *testing.T, cfg Config) *Engine {
	t.Helper()
	e, err := Open(testDict(t), nil, cfg)
	if err != nil {
		t.Fatalf("打开引擎: %v", err)
	}
	return e
}

func key(r rune) Key { return Key{Code: keyCodeForRune(r), Rune: r, Down: true} }

func keyCodeForRune(r rune) KeyCode {
	switch {
	case r >= 'a' && r <= 'z':
		return KeyA + KeyCode(r-'a')
	case r >= 'A' && r <= 'Z':
		return KeyA + KeyCode(r-'A')
	case r >= '0' && r <= '9':
		return Key0 + KeyCode(r-'0')
	}
	return KeyPunct
}

// typeLetters 逐字母输入并返回最后一个响应。
func typeLetters(s *Session, letters string) Response {
	var resp Response
	for _, r := range letters {
		k := key(r)
		resp = s.Handle(Event{Key: &k})
	}
	return resp
}

func press(s *Session, code KeyCode) Response {
	k := Key{Code: code, Down: true}
	return s.Handle(Event{Key: &k})
}

func pressRune(s *Session, r rune) Response {
	k := Key{Code: KeyPunct, Rune: r, Down: true}
	return s.Handle(Event{Key: &k})
}

func texts(r Response) []string {
	out := make([]string, len(r.Candidates))
	for i, c := range r.Candidates {
		out[i] = c.Text
	}
	return out
}

func equalStrings(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}
