package engine

import (
	"reflect"
	"testing"
)

func TestFuzzyGroupsApplyOnlyAtSyllableBoundaries(t *testing.T) {
	fuzzy := newTestEngine(t, Config{PageSize: 5, FuzzyGroups: [][]string{{"s", "sh"}}})
	s := fuzzy.NewSession(SessionMeta{})
	if got := texts(typeLetters(s, "shanghai")); got[0] != "上海" {
		t.Fatalf("正常全拼被模糊音干扰: %v", got)
	}
	// 声母 s/sh 等价：sang 应能命中 shang 系列音节。
	s = fuzzy.NewSession(SessionMeta{})
	if got := texts(typeLetters(s, "sang")); len(got) == 0 {
		t.Fatal("模糊音未生效: 输入 sang 无候选")
	}

	strict := newTestEngine(t, Config{PageSize: 5})
	s = strict.NewSession(SessionMeta{})
	if got := texts(typeLetters(s, "sang")); len(got) != 0 {
		t.Fatalf("未配置模糊音时不应命中 shang: %v", got)
	}
}

func TestAbbreviationDoesNotShadowFullSpelling(t *testing.T) {
	e := newTestEngine(t, Config{PageSize: 5})
	// "zh" 是 zh/zha/zhan… 的前缀，必须按全拼前缀解释，而不是展开简拼。
	s := e.NewSession(SessionMeta{})
	got := texts(typeLetters(s, "zh"))
	want := []string{"这", "中", "着", "之", "只"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("前缀输入被简拼劫持: %v", got)
	}
}

func TestSingleLetterQueryUsesPrefix(t *testing.T) {
	e := newTestEngine(t, Config{PageSize: 5})
	s := e.NewSession(SessionMeta{})
	resp := typeLetters(s, "w")
	if len(resp.Candidates) == 0 {
		t.Fatal("单字母输入应给出前缀候选")
	}
	for _, c := range resp.Candidates {
		if c.Comment == "" {
			t.Fatalf("候选缺少拼音注释: %+v", c)
		}
	}
}

func TestUnknownSyllableFallsBackToLetters(t *testing.T) {
	e := newTestEngine(t, Config{PageSize: 5})
	s := e.NewSession(SessionMeta{})
	resp := typeLetters(s, "qwxz")
	if resp.Preedit != "qwxz" || len(resp.Candidates) != 0 {
		t.Fatalf("未知拼写应只保留字母: %+v", resp)
	}
	if resp.Commit = press(s, KeySpace).Commit; resp.Commit != "qwxz" {
		t.Fatalf("无候选时空格应原样上屏，得到 %q", resp.Commit)
	}
}

func TestPairedPunctuationNests(t *testing.T) {
	e := newTestEngine(t, Config{PageSize: 5})
	s := e.NewSession(SessionMeta{})
	s.Handle(Event{Toggle: &chineseFull})

	steps := []struct {
		input rune
		want  string
	}{
		{'"', "“"},
		{'(', "（"},
		{')', "）"},
		{'"', "”"},
		{'"', "“"},
		{'"', "”"},
	}
	for _, step := range steps {
		resp := pressRune(s, step.input)
		if resp.Commit != step.want {
			t.Fatalf("%q 上屏 %q，期望 %q", step.input, resp.Commit, step.want)
		}
	}
	// 闭合标点没有对应开标点时仍输出全角闭合字符。
	if got := pressRune(s, ')').Commit; got != "）" {
		t.Fatalf("孤立右括号上屏 %q", got)
	}
}

func TestPunctuationPairStackResetsOnSessionClose(t *testing.T) {
	e := newTestEngine(t, Config{PageSize: 5})
	s := e.NewSession(SessionMeta{})
	s.Handle(Event{Toggle: &chineseFull})
	pressRune(s, '"')
	s.Close()

	next := e.NewSession(SessionMeta{})
	next.Handle(Event{Toggle: &chineseFull})
	if got := pressRune(next, '"').Commit; got != "“" {
		t.Fatalf("新会话的配对栈应重置，得到 %q", got)
	}
}

func TestEnglishModePassesEverythingThrough(t *testing.T) {
	e := newTestEngine(t, Config{PageSize: 5})
	s := e.NewSession(SessionMeta{})
	s.Handle(Event{Toggle: &english})
	for _, r := range "hello" {
		resp := press(s, keyCodeForRune(r))
		if resp.Consume || resp.Preedit != "" || resp.Commit != "" {
			t.Fatalf("英文模式字母 %q 未被直通: %+v", r, resp)
		}
	}
	if resp := press(s, KeySpace); resp.Consume {
		t.Fatal("英文模式空格应交给应用")
	}
}

func TestHandleIgnoresEmptyEvent(t *testing.T) {
	e := newTestEngine(t, Config{PageSize: 5})
	s := e.NewSession(SessionMeta{})
	resp := s.Handle(Event{})
	if resp.Consume || resp.Preedit != "" || !resp.Mode.Chinese {
		t.Fatalf("空事件响应异常: %+v", resp)
	}
}
