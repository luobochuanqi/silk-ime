package engine

// 智能标点只在全角模式下替换；半角模式交由应用处理。成对标点（引号、括号、
// 书名号）用栈记录尚未闭合的右半字符，栈随会话存续，跨多次上屏仍能正确配对。

type punctRule struct {
	closer rune   // 成对标点期望的右半字符；0 表示非成对
	text   string // 全角替换文本
}

var punctRules = map[rune]punctRule{
	'"':  {'”', "“"},
	'\'': {'’', "‘"},
	'(':  {')', "（"},
	')':  {0, "）"},
	'[':  {']', "【"},
	']':  {0, "】"},
	'{':  {'}', "｛"},
	'}':  {0, "｝"},
	'<':  {'>', "《"},
	'>':  {0, "》"},
	',':  {0, "，"},
	'.':  {0, "。"},
	'?':  {0, "？"},
	'!':  {0, "！"},
	';':  {0, "；"},
	':':  {0, "："},
	'\\': {0, "、"},
}

// smartPunct 返回标点上屏文本并维护配对栈：开引号在栈顶为自身右半时改出右半，
// 闭合标点仅在栈顶匹配时出栈，避免不合法的嵌套污染后续配对。
func (s *Session) smartPunct(r rune) string {
	rule, ok := punctRules[r]
	if !ok {
		return string(r)
	}
	if r == '"' || r == '\'' {
		if n := len(s.pairs); n > 0 && s.pairs[n-1] == rule.closer {
			s.pairs = s.pairs[:n-1]
			return string(rule.closer)
		}
		s.pairs = append(s.pairs, rule.closer)
		return rule.text
	}
	if rule.closer != 0 {
		s.pairs = append(s.pairs, rule.closer)
		return rule.text
	}
	if n := len(s.pairs); n > 0 && s.pairs[n-1] == r {
		s.pairs = s.pairs[:n-1]
	}
	return rule.text
}

// punct 处理标点键：全角模式下与组合一起上屏，半角模式原样放行。
func (s *Session) punct(k Key) Response {
	r := k.Rune
	if r == 0 {
		// 没有字符载荷的标点键无法替换，按未处理事件放行。
		resp := s.view()
		resp.Consume = false
		return resp
	}
	if !s.mode.FullPunct {
		if len(s.input) == 0 {
			resp := s.view()
			resp.Consume = false
			return resp
		}
		// 组合中按下半角标点：先上屏首选，再按原字符上屏标点，与全角路径保持一致，
		// 避免应用侧再插入一次导致重复。
		return s.commitPunct(int64(k.TS), string(r))
	}
	return s.commitPunct(int64(k.TS), s.smartPunct(r))
}

// commitPunct 把组合的首选与标点一起上屏；无组合时只上屏标点。
func (s *Session) commitPunct(ts int64, text string) Response {
	prefix := s.commitFirstText(ts)
	resp := s.view()
	resp.Commit = prefix + text
	resp.Consume = true
	return resp
}

// commitFirstText 在当前组合存在时取首选上屏并返回其上屏文本，无组合返回空串。
func (s *Session) commitFirstText(ts int64) string {
	if len(s.input) == 0 {
		return ""
	}
	cands := s.current()
	if len(cands) == 0 {
		raw := string(s.input)
		s.clear()
		return raw
	}
	chosen := cands[0]
	s.clear()
	s.record(chosen, ts)
	return chosen.text
}
