package engine

import (
	"reflect"
	"testing"
)

// golden 描述一条黄金用例：可选的前置动作、输入字母串，以及可选的后续动作。
// 字段为 nil 表示不校验该字段；wantPreedit 指向空串表示断言无组合。
type golden struct {
	name    string
	setup   func(*Session) Response
	letters string
	action  func(*Session) Response

	wantPreedit *string
	wantCands   []string
	wantCommit  *string
	wantConsume *bool
}

func str(s string) *string { return &s }

func space(s *Session) Response     { return press(s, KeySpace) }
func enter(s *Session) Response     { return press(s, KeyEnter) }
func esc(s *Session) Response       { return press(s, KeyEsc) }
func backspace(s *Session) Response { return press(s, KeyBackspace) }
func pageDown(s *Session) Response  { return press(s, KeyPageDown) }
func pageUp(s *Session) Response    { return press(s, KeyPageUp) }
func digit(n KeyCode) func(*Session) Response {
	return func(s *Session) Response { return press(s, n) }
}
func selectAt(n int) func(*Session) Response {
	return func(s *Session) Response { return s.Handle(Event{Select: &n}) }
}
func punct(r rune) func(*Session) Response {
	return func(s *Session) Response { return pressRune(s, r) }
}
func toggle(m Mode) func(*Session) Response {
	return func(s *Session) Response { return s.Handle(Event{Toggle: &m}) }
}
func focusOut(s *Session) Response { return s.Handle(Event{FocusOut: true}) }

var (
	chineseFull = Mode{Chinese: true, FullPunct: true}
	english     = Mode{Chinese: false}
)

// chain 依次执行多个动作并返回最后一个响应。
func chain(actions ...func(*Session) Response) func(*Session) Response {
	return func(s *Session) Response {
		var resp Response
		for _, a := range actions {
			resp = a(s)
		}
		return resp
	}
}

func TestGolden(t *testing.T) {
	cfg := Config{PageSize: 5, FuzzyGroups: [][]string{{"z", "zh"}, {"c", "ch"}, {"s", "sh"}, {"l", "n"}}}

	cases := []golden{
		// 全拼整句与整词
		{name: "全拼单字", letters: "ni", wantCands: []string{"你", "里"}},
		{name: "全拼整词", letters: "nihao", wantCands: []string{"你好", "里好"}},
		{name: "全拼多字词", letters: "zhongguo", wantCands: []string{"中国", "中过", "种国", "总国", "种过"}},
		{name: "全拼国名", letters: "riben", wantCands: []string{"日本"}},
		{name: "三字词", letters: "shurufa", wantCands: []string{"输入法", "输入发"}},
		{name: "五字整句", letters: "shengrikuaile", wantCands: []string{"生日快乐"}},
		{name: "六字整句", letters: "wobuzhidao", wantCands: []string{"我不知道", "我不子到", "我不自到", "我不之到", "我不只到"}},
		{name: "地名", letters: "beijing", wantCands: []string{"北京"}},
		{name: "前缀候选", letters: "zhong", wantCands: []string{"中", "种", "总"}},
		{name: "三字地名", letters: "zhongguoren", wantCands: []string{"中国人", "中过人", "种国人", "总国人", "种过人"}},
		{name: "两词整句", letters: "nihaoshijie", wantCands: []string{"你好世界", "里好世界"}},
		{name: "两词整句二", letters: "womendediannao", wantCands: []string{"我们的电脑", "我们得电脑"}},
		// 简拼
		{name: "简拼双字", letters: "nh", wantCands: []string{"你好", "您好"}},
		{name: "简拼三字", letters: "srf", wantCands: []string{"输入法"}},
		{name: "简拼三字共存", letters: "zj", wantCands: []string{"再见"}},
		// 模糊音
		{name: "模糊音 z/zh", letters: "zongguo", wantCands: []string{"中国", "中过", "种国", "总国", "种过"}},
		{name: "模糊音 l/n", letters: "lihao", wantCands: []string{"你好", "里好"}},
		{name: "模糊音 c/ch", letters: "congqing", wantCands: []string{"重庆", "从情"}},
		// 同音候选排序
		{name: "同音按频次", letters: "shi", wantCands: []string{"是", "时", "事"}},
		{name: "同音多候选", letters: "yi", wantCands: []string{"一", "以", "意"}},
		// 上屏语义
		{name: "空格上屏首选", letters: "nihao", action: space, wantCommit: str("你好"), wantPreedit: str(""), wantConsume: new(true)},
		{name: "数字键选第二", letters: "zhongguo", action: digit(Key2), wantCommit: str("中过")},
		{name: "数字键选首位", letters: "shijie", action: digit(Key1), wantCommit: str("世界")},
		{name: "选择事件选第三", letters: "shi", action: selectAt(2), wantCommit: str("事")},
		{name: "Enter 原样上屏", letters: "niha", action: enter, wantCommit: str("niha")},
		{name: "Esc 清空", letters: "nihao", action: esc, wantPreedit: str(""), wantCands: []string{}, wantCommit: str("")},
		{name: "退格删字母", letters: "nihao", action: backspace, wantPreedit: str("niha"), wantCands: []string{"你好", "你还", "里好", "里还"}},
		{name: "退格至空", letters: "n", action: backspace, wantPreedit: str(""), wantCands: []string{}},
		{name: "退格跨音节", letters: "zhongguo", action: chain(backspace, backspace), wantPreedit: str("zhongg")},
		{name: "数字键选末位", letters: "zhongguo", action: digit(Key4), wantCommit: str("总国")},
		{name: "空格上屏多字词", letters: "wobuzhidao", action: space, wantCommit: str("我不知道")},
		{name: "越界数字键保持组合", letters: "nihao", action: digit(Key9), wantPreedit: str("nihao"), wantCommit: str("")},
		{name: "越界选择不消耗", letters: "nihao", action: selectAt(9), wantPreedit: str("nihao"), wantConsume: new(false)},
		{name: "空缓冲空格不消耗", action: space, wantConsume: new(false)},
		{name: "空缓冲退格不消耗", action: backspace, wantConsume: new(false)},
		{name: "空缓冲翻页不消耗", action: pageDown, wantConsume: new(false)},
		// 翻页
		{name: "翻页下标", letters: "z", action: pageDown, wantPreedit: str("z"), wantCands: []string{"只"}},
		{name: "翻页回退", letters: "z", action: chain(pageDown, pageUp), wantPreedit: str("z"), wantCands: []string{"在", "这", "中", "着", "之"}},
		{name: "翻页越界保持", letters: "shi", action: pageDown, wantPreedit: str("shi"), wantCands: []string{"是", "时", "事"}},
		// 智能标点
		{name: "全角逗号", letters: "nihao", setup: toggle(chineseFull), action: punct(','), wantCommit: str("你好，"), wantPreedit: str("")},
		{name: "成对引号开启", setup: toggle(chineseFull), action: punct('"'), wantCommit: str("“")},
		{name: "成对引号闭合", setup: toggle(chineseFull), action: chain(punct('"'), punct('"')), wantCommit: str("”")},
		{name: "成对括号", setup: toggle(chineseFull), action: chain(punct('('), punct(')')), wantCommit: str("）")},
		{name: "嵌套括号配对", setup: toggle(chineseFull), action: chain(punct('('), punct('('), punct(')'), punct(')')), wantCommit: str("）")},
		{name: "问号全角", setup: toggle(chineseFull), action: punct('?'), wantCommit: str("？")},
		{name: "书名号", setup: toggle(chineseFull), action: chain(punct('<'), punct('>')), wantCommit: str("》")},
		{name: "半角标点直通", action: punct(','), wantConsume: new(false), wantCommit: str("")},
		{name: "半角标点组合上屏", letters: "nihao", action: punct(','), wantCommit: str("你好,")},
		// 模式切换
		{name: "切英文后字母直通", setup: toggle(english), action: func(s *Session) Response { return press(s, KeyH) }, wantConsume: new(false), wantPreedit: str("")},
		{name: "切换时原样上屏组合", letters: "ni", action: toggle(english), wantCommit: str("ni")},
		{name: "模式快照", setup: toggle(chineseFull), wantCands: []string{}},
		// 失焦
		{name: "失焦原样上屏", letters: "zhongguo", action: focusOut, wantCommit: str("zhongguo"), wantPreedit: str("")},
		{name: "失焦无组合不消耗", action: focusOut, wantConsume: new(false)},
		// 无候选
		{name: "无匹配仍保留字母", letters: "qqqq", wantPreedit: str("qqqq"), wantCands: []string{}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			// 每条用例独立引擎：用户词频是跨会话共享的，共享引擎会让用例互相影响。
			s := newTestEngine(t, cfg).NewSession(SessionMeta{App: "test"})
			defer s.Close()
			var resp Response
			if tc.setup != nil {
				resp = tc.setup(s)
			}
			if tc.letters != "" {
				resp = typeLetters(s, tc.letters)
			}
			if tc.action != nil {
				resp = tc.action(s)
			}
			if tc.wantPreedit != nil && resp.Preedit != *tc.wantPreedit {
				t.Errorf("preedit = %q，期望 %q", resp.Preedit, *tc.wantPreedit)
			}
			if tc.wantCands != nil {
				if got := texts(resp); !reflect.DeepEqual(got, tc.wantCands) {
					t.Errorf("候选 = %v，期望 %v", got, tc.wantCands)
				}
			}
			if tc.wantCommit != nil && resp.Commit != *tc.wantCommit {
				t.Errorf("commit = %q，期望 %q", resp.Commit, *tc.wantCommit)
			}
			if tc.wantConsume != nil && resp.Consume != *tc.wantConsume {
				t.Errorf("consume = %v，期望 %v", resp.Consume, *tc.wantConsume)
			}
		})
	}
}

func TestCaretAndComment(t *testing.T) {
	e := newTestEngine(t, Config{PageSize: 5})
	s := e.NewSession(SessionMeta{})
	resp := typeLetters(s, "nihao")
	if resp.Caret != 5 {
		t.Fatalf("caret = %d，期望 5", resp.Caret)
	}
	if len(resp.Candidates) == 0 || resp.Candidates[0].Comment == "" {
		t.Fatalf("候选缺少注释: %+v", resp.Candidates)
	}
	if !resp.Consume || !resp.Mode.Chinese {
		t.Fatalf("组合态应为中文且消耗按键: %+v", resp)
	}
}

func TestModeSurvivesSession(t *testing.T) {
	e := newTestEngine(t, Config{PageSize: 5})
	s := e.NewSession(SessionMeta{})
	s.Handle(Event{Toggle: &english})
	resp := typeLetters(s, "ni")
	if resp.Mode.Chinese || resp.Preedit != "" {
		t.Fatalf("英文模式下不应组字: %+v", resp)
	}
	s.Handle(Event{Toggle: &chineseFull})
	resp = typeLetters(s, "ni")
	if !resp.Mode.Chinese || !resp.Mode.FullPunct || resp.Preedit != "ni" {
		t.Fatalf("切回中文后应恢复全角组字: %+v", resp)
	}
}
