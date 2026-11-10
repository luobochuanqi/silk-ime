package engine

import (
	"sort"
	"strings"
	"sync"

	"github.com/luobochuanqi/silk-ime/internal/dict"
)

// maxLetters 约束拼写缓冲，保证极端输入下每次 Handle 的耗时与内存有界。
const maxLetters = 64

// Engine 持有只读词库、模糊音展开表与用户词频，可被多个会话共享。
type Engine struct {
	d   *dict.Dictionary
	cfg Config

	accept map[string][]string // 输入拼写 → 规范音节
	abbrev map[string][]uint32 // 音节首字母串 → 词条 id（简拼）
	// 任一声节的前缀集合：落在其中的输入按全拼解释，不展开简拼。
	spellPrefix map[string]struct{}

	mu sync.Mutex
	// 用户词频按词条 id 索引：词典是只读的，id 在整个引擎生命周期内稳定，
	// 用字符串键会在每次候选打分时产生拼键分配。
	freq  map[uint32]int
	delta []UserEntry
}

// Session 是一个文档的输入状态。非并发安全，由调用方保证串行调用。
type Session struct {
	e     *Engine
	mode  Mode
	input []rune
	page  int
	// 成对标点栈保存尚未闭合的右半字符；跨上屏保留，随会话重建而清空。
	pairs []rune

	cached    []cand
	cachedFor string
}

func defaultConfig(cfg Config) Config {
	if cfg.PageSize <= 0 {
		cfg.PageSize = 5
	}
	return cfg
}

// Open 加载词库字节并并入用户词频。data 只读，Engine 不持有其引用。
func Open(data []byte, user []UserEntry, cfg Config) (*Engine, error) {
	d, err := dict.Open(data)
	if err != nil {
		return nil, err
	}
	e := &Engine{d: d, cfg: defaultConfig(cfg), freq: make(map[uint32]int, len(user))}
	e.buildFuzzy()
	e.buildAbbrev()
	for _, u := range user {
		if u.Word == "" || u.Freq <= 0 {
			continue
		}
		// 只有词典内存在的词条才可能成为候选，其余条目在打分时无从命中。
		for _, id := range d.Lookup(u.Pinyin) {
			if d.Word(id).Text == u.Word {
				e.freq[id] += u.Freq
				break
			}
		}
	}
	return e, nil
}

// buildFuzzy 把模糊音组展开为「输入拼写 → 规范音节」映射。只允许在音节首或音节尾
// 做一次整段替换：声母组（z/zh、n/l…）只在开头生效，韵母组（an/ang…）只在结尾
// 生效，避免 "shang" 被 s→sh 之类的子串替换污染成非法拼写。每组只替换一次，
// 不叠加多组，保持映射表规模有界。
func (e *Engine) buildFuzzy() {
	e.accept = make(map[string][]string, len(e.d.Syllables())*2)
	for _, syl := range e.d.Syllables() {
		e.accept[syl] = append(e.accept[syl], syl)
		for _, g := range e.cfg.FuzzyGroups {
			for _, to := range g {
				for _, from := range g {
					if to == from {
						continue
					}
					if strings.HasPrefix(syl, to) {
						e.addFuzzy(from+syl[len(to):], syl)
					}
					if strings.HasSuffix(syl, to) {
						e.addFuzzy(syl[:len(syl)-len(to)]+from, syl)
					}
				}
			}
		}
	}
	for k := range e.accept {
		sort.Strings(e.accept[k])
	}
}

func (e *Engine) addFuzzy(spell, canon string) {
	if spell == "" {
		return
	}
	v := e.accept[spell]
	if len(v) > 0 && v[len(v)-1] == canon {
		return
	}
	e.accept[spell] = append(v, canon)
}

// buildAbbrev 建立简拼索引：每个词按其音节首字母串登记，多字词优先，便于
// 简拼输入直接命中整词。
func (e *Engine) buildAbbrev() {
	e.abbrev = make(map[string][]uint32, e.d.Len())
	e.spellPrefix = make(map[string]struct{}, len(e.d.Syllables())*maxSpell)
	for _, syl := range e.d.Syllables() {
		for i := 1; i <= len(syl); i++ {
			e.spellPrefix[syl[:i]] = struct{}{}
		}
	}
	for id := range uint32(e.d.Len()) {
		w := e.d.Word(id)
		var b strings.Builder
		for _, syl := range strings.Fields(w.Key) {
			b.WriteByte(syl[0])
		}
		k := b.String()
		e.abbrev[k] = append(e.abbrev[k], id)
	}
}

// NewSession 创建一个独立会话，初始为中文、半角标点。meta 目前不改变行为，
// 保留该参数供后续按应用统计。
func (e *Engine) NewSession(meta SessionMeta) *Session {
	_ = meta
	return &Session{e: e, mode: Mode{Chinese: true}}
}

// DrainUserDelta 取走自上次调用以来的用户词频增量。
func (e *Engine) DrainUserDelta() []UserEntry {
	e.mu.Lock()
	defer e.mu.Unlock()
	out := e.delta
	e.delta = nil
	return out
}

func (e *Engine) userFreq(id uint32) int64 {
	e.mu.Lock()
	defer e.mu.Unlock()
	return int64(e.freq[id])
}

// bump 记录一次上屏，词频 +1 并累积到增量。
func (e *Engine) bump(w wordRef, ts int64) {
	e.mu.Lock()
	defer e.mu.Unlock()
	e.freq[w.id]++
	e.delta = append(e.delta, UserEntry{Word: w.text, Pinyin: w.key, Freq: 1, TS: ts})
}

// Handle 处理一次事件并返回响应。事件按 Key / Select / Page / Toggle / FocusOut
// 的优先顺序解释，同一响应只走其中一条分支。
func (s *Session) Handle(ev Event) Response {
	switch {
	case ev.FocusOut:
		return s.focusOut()
	case ev.Toggle != nil:
		return s.toggle(*ev.Toggle)
	case ev.Select != nil:
		return s.selectAt(*ev.Select, 0)
	case ev.Page != nil:
		return s.turnPage(*ev.Page)
	case ev.Key != nil:
		return s.key(*ev.Key)
	}
	return s.view()
}

// Close 结束会话；未上屏的字母按原样上屏，与失焦策略一致。
func (s *Session) Close() { s.focusOut() }

func (s *Session) focusOut() Response {
	resp := s.view()
	if len(s.input) > 0 {
		resp.Commit = string(s.input)
	}
	s.clear()
	resp.Preedit, resp.Caret, resp.Candidates, resp.PageIndex = "", 0, nil, 0
	resp.Consume = resp.Commit != ""
	return resp
}

func (s *Session) toggle(m Mode) Response {
	var commit string
	if len(s.input) > 0 {
		commit = string(s.input)
	}
	s.clear()
	s.mode = m
	resp := s.view()
	resp.Commit = commit
	resp.Consume = commit != ""
	return resp
}

func (s *Session) selectAt(index int, ts int64) Response {
	cands := s.current()
	start := s.page * s.e.cfg.PageSize
	if index < 0 || start+index >= len(cands) {
		resp := s.view()
		resp.Consume = false
		return resp
	}
	chosen := cands[start+index]
	commit := chosen.text
	s.clear()
	resp := s.view()
	resp.Commit = commit
	resp.Consume = true
	s.record(chosen, ts)
	return resp
}

func (s *Session) turnPage(delta int) Response {
	if len(s.input) == 0 {
		resp := s.view()
		resp.Consume = false
		return resp
	}
	pages := s.pageCount()
	next := s.page + delta
	if next < 0 || next >= pages {
		return s.view()
	}
	s.page = next
	return s.view()
}

func (s *Session) pageCount() int {
	cands := s.current()
	return (len(cands) + s.e.cfg.PageSize - 1) / s.e.cfg.PageSize
}

func (s *Session) current() []cand {
	key := string(s.input)
	if s.cachedFor == key && s.cached != nil {
		return s.cached
	}
	s.cached = s.e.collect(key)
	s.cachedFor = key
	return s.cached
}

func (s *Session) clear() {
	s.input = s.input[:0]
	s.page = 0
	s.cached, s.cachedFor = nil, ""
}

func (s *Session) record(c cand, ts int64) {
	for _, w := range c.words {
		if w.key == "" {
			continue
		}
		s.e.bump(w, ts)
	}
}

func (s *Session) key(k Key) Response {
	if !k.Down {
		resp := s.view()
		resp.Consume = s.mode.Chinese && len(s.input) > 0
		return resp
	}
	if !s.mode.Chinese {
		return s.englishKey()
	}
	switch k.Code {
	case KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM,
		KeyN, KeyO, KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ:
		return s.letter(k)
	case Key0, Key1, Key2, Key3, Key4, Key5, Key6, Key7, Key8, Key9:
		digit := int(k.Code - Key1)
		if k.Code == Key0 {
			digit = 9
		}
		if len(s.input) == 0 {
			resp := s.view()
			resp.Consume = false
			return resp
		}
		if s.page*s.e.cfg.PageSize+digit >= len(s.current()) {
			// 组合中按下越界数字键仍由输入法吞掉，否则应用会插入阿拉伯数字。
			return s.view()
		}
		return s.selectAt(digit, int64(k.TS))
	case KeySpace:
		return s.commitFirst(int64(k.TS))
	case KeyEnter:
		return s.commitRaw()
	case KeyEsc:
		had := len(s.input) > 0
		s.clear()
		resp := s.view()
		resp.Consume = had
		return resp
	case KeyBackspace:
		if len(s.input) == 0 {
			resp := s.view()
			resp.Consume = false
			return resp
		}
		s.deleteLast()
		return s.view()
	case KeyPageUp:
		return s.turnPage(-1)
	case KeyPageDown:
		return s.turnPage(1)
	case KeyPunct:
		return s.punct(k)
	}
	return s.view()
}

func (s *Session) englishKey() Response {
	resp := s.view()
	resp.Consume = false
	return resp
}

func (s *Session) letter(k Key) Response {
	r := k.Rune
	if r >= 'A' && r <= 'Z' {
		r += 'a' - 'A'
	}
	if r < 'a' || r > 'z' {
		// Rune 缺省或非字母时以语义键码为准，保证宿主只填 Code 也能工作。
		r = rune('a' + int(k.Code-KeyA))
	}
	if len(s.input) < maxLetters {
		s.input = append(s.input, r)
		s.page = 0
	}
	return s.view()
}

// deleteLast 删除缓冲末尾字母；字母删空后回到无组合状态。
func (s *Session) deleteLast() {
	if len(s.input) > 0 {
		s.input = s.input[:len(s.input)-1]
		s.page = 0
	}
}

// commitFirst 上屏当前首选；无候选时退回原样字母。
func (s *Session) commitFirst(ts int64) Response {
	if len(s.input) == 0 {
		resp := s.view()
		resp.Consume = false
		return resp
	}
	commit := s.commitFirstText(ts)
	resp := s.view()
	resp.Commit = commit
	resp.Consume = true
	return resp
}

func (s *Session) commitRaw() Response {
	if len(s.input) == 0 {
		resp := s.view()
		resp.Consume = false
		return resp
	}
	commit := string(s.input)
	s.clear()
	resp := s.view()
	resp.Commit = commit
	resp.Consume = true
	return resp
}

func (s *Session) view() Response {
	cands := s.current()
	pages := 1
	if len(cands) > 0 {
		pages = s.pageCount()
	}
	if s.page >= pages {
		s.page = pages - 1
	}
	if s.page < 0 {
		s.page = 0
	}
	start := s.page * s.e.cfg.PageSize
	end := start + s.e.cfg.PageSize
	if end > len(cands) {
		end = len(cands)
	}
	var page []Candidate
	if start < end {
		page = make([]Candidate, 0, end-start)
		for _, c := range cands[start:end] {
			page = append(page, Candidate{Text: c.text, Comment: c.comment})
		}
	}
	return Response{
		Consume:    s.mode.Chinese && len(s.input) > 0,
		Preedit:    string(s.input),
		Caret:      len(s.input),
		Candidates: page,
		PageIndex:  s.page,
		Mode:       s.mode,
	}
}
