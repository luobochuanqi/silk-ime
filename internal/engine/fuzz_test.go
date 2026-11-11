package engine

import (
	"testing"
)

// FuzzHandle 断言任意 rune 流与动作序列都不会让 Handle panic，且预编辑长度有界。
func FuzzHandle(f *testing.F) {
	e, err := Open(testDict(f), nil, Config{PageSize: 5, FuzzyGroups: [][]string{{"z", "zh"}, {"l", "n"}}})
	if err != nil {
		f.Fatalf("打开引擎: %v", err)
	}
	f.Add("nihao")
	f.Add("")
	f.Add("zhongguorrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrr")
	f.Add("😀a\u0000b")
	f.Fuzz(func(t *testing.T, input string) {
		s := e.NewSession(SessionMeta{App: "fuzz"})
		defer s.Close()
		var resp Response
		for _, r := range input {
			k := Key{Code: KeyPunct, Rune: r, Down: true}
			resp = s.Handle(Event{Key: &k})
			if len(resp.Preedit) > maxLetters {
				t.Fatalf("预编辑长度 %d 超出上限", len(resp.Preedit))
			}
			if resp.Caret != len([]rune(resp.Preedit)) {
				t.Fatalf("caret %d 与预编辑长度不一致", resp.Caret)
			}
		}
		idx := len(resp.Candidates)
		s.Handle(Event{Select: &idx})
		delta := 1
		s.Handle(Event{Page: &delta})
		s.Handle(Event{FocusOut: true})
	})
}
