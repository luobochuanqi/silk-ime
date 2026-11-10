package engine

import (
	"sort"
	"strings"

	"github.com/luobochuanqi/silk-ime/internal/dict"
)

// 音节拼写上限，覆盖 zhuang/chuang/shuang 一类最长拼音。
const maxSpell = 6

// 切分路径、每级状态与同键候选的上限，保证任意输入下 Handle 的耗时与内存有界。
const (
	maxPaths  = 48
	maxStates = 6
	maxAlts   = 6
)

type token struct {
	spell   string // 用户输入的字母
	canon   string // 规范音节；partial 时为空
	partial bool   // 缓冲末尾的未完成音节，按前缀检索
}

type wordRef struct {
	id   uint32
	text string
	key  string
}

type cand struct {
	text    string
	comment string
	cost    int64
	words   []wordRef
	// broken 表示该候选含有词典缺失、退回原样字母的片段；只有整页都是这类候选时
	// 才展示，避免伪词挤占正常候选页。
	broken bool
}

type levelState struct {
	text  string
	cost  int64
	words []wordRef
}

// spellings 枚举缓冲字母的全部全拼切分，末尾允许未完成音节；模糊音已并入
// accept 表，因此这里不再做二次查询。
func (e *Engine) spellings(letters string) [][]token {
	var paths [][]token
	var cur []token
	var walk func(pos int)
	walk = func(pos int) {
		if len(paths) >= maxPaths {
			return
		}
		if pos == len(letters) {
			paths = append(paths, append([]token(nil), cur...))
			return
		}
		for l := 1; l <= maxSpell && pos+l <= len(letters); l++ {
			part := letters[pos : pos+l]
			end := pos+l == len(letters)
			for _, canon := range e.accept[part] {
				cur = append(cur, token{spell: part, canon: canon})
				walk(pos + l)
				cur = cur[:len(cur)-1]
			}
			if end {
				if _, complete := e.accept[part]; !complete {
					cur = append(cur, token{spell: part, partial: true})
					walk(pos + l)
					cur = cur[:len(cur)-1]
				}
			}
		}
	}
	walk(0)
	return paths
}

// pathCandidates 对一条音节路径做逐级束搜索：每个位置优先取最长可行词长，
// 词内同音项全部保留；词典缺失的单音节回退为原样字母，避免整句无候选。
func (e *Engine) pathCandidates(toks []token) []cand {
	n := len(toks)
	levels := make([][]levelState, n+1)
	levels[0] = []levelState{{}}
	for j := 0; j < n; j++ {
		if len(levels[j]) == 0 {
			continue
		}
		for _, st := range levels[j] {
			for l := n - j; l >= 1; l-- {
				tail := toks[j : j+l]
				if tail[l-1].partial {
					if l != n-j {
						continue
					}
					key := joinKey(tail[:l-1])
					if key != "" {
						key += " "
					}
					key += tail[l-1].spell
					for _, id := range e.prefixIDs(key, l) {
						w := e.d.Word(id)
						levels[n] = append(levels[n], st.appendWord(w, id, e.costOf(id)))
					}
					continue
				}
				key := joinKey(tail)
				ids := e.d.ExactWords(key, l)
				for i, id := range ids {
					if i >= maxAlts {
						break
					}
					w := e.d.Word(id)
					levels[j+l] = append(levels[j+l], st.appendWord(w, id, e.costOf(id)))
				}
				if l == 1 && len(ids) == 0 {
					levels[j+1] = append(levels[j+1], levelState{
						text:  st.text + tail[0].spell,
						cost:  st.cost + fallbackCost,
						words: appendWord(st.words, wordRef{text: tail[0].spell}),
					})
				}
			}
		}
		for k := j + 1; k <= n; k++ {
			levels[k] = trimStates(levels[k])
		}
	}
	out := make([]cand, 0, len(levels[n]))
	for _, st := range levels[n] {
		out = append(out, cand{
			text:    st.text,
			comment: commentOf(st.words),
			cost:    st.cost,
			words:   st.words,
			broken:  isBroken(st.words),
		})
	}
	return out
}

func (e *Engine) abbrevCandidates(letters string, byText map[string]cand) {
	for _, id := range e.abbrev[letters] {
		w := e.d.Word(id)
		c := cand{
			text:    w.Text,
			comment: w.Key,
			cost:    e.costOf(id),
			words:   []wordRef{{text: w.Text, key: w.Key}},
		}
		if prev, ok := byText[c.text]; ok && prev.cost <= c.cost {
			continue
		}
		byText[c.text] = c
	}
}

func (st levelState) appendWord(w dict.Word, id uint32, cost int64) levelState {
	return levelState{
		text:  st.text + w.Text,
		cost:  st.cost + cost,
		words: appendWord(st.words, wordRef{id: id, text: w.Text, key: w.Key}),
	}
}

// gateWordCount 丢弃切词数明显多于最佳候选的路径：这类结果通常是把一个音节拆成
// 单字母音节再凑词（如 "上传" 被拆成"上 出 啊 你"），对用户没有价值。只放开一档
// 余量，保留"中 国"与"中国"这类真实竞争。
func gateWordCount(cands []cand) []cand {
	if len(cands) == 0 {
		return cands
	}
	best := cands[0]
	for _, c := range cands[1:] {
		if c.cost < best.cost {
			best = c
		}
	}
	out := cands[:0]
	for _, c := range cands {
		if len(c.words) <= len(best.words)+1 {
			out = append(out, c)
		}
	}
	return out
}

// isBroken 判断切分中是否含词典缺失片段。空 key 的词引用只来自原样字母回退。
func isBroken(words []wordRef) bool {
	for _, w := range words {
		if w.key == "" {
			return true
		}
	}
	return false
}

// prefixIDs 取前缀检索结果的前若干条，返回的是倒排切片的下标窗口，不复制。
func (e *Engine) prefixIDs(key string, syllables int) []uint32 {
	ids := e.d.PrefixLookup(key, syllables)
	return ids[:min(len(ids), maxAlts)]
}

func trimStates(states []levelState) []levelState {
	if len(states) <= maxStates {
		return states
	}
	seen := make(map[string]struct{}, len(states))
	out := states[:0]
	for _, s := range states {
		if _, ok := seen[s.text]; ok {
			continue
		}
		seen[s.text] = struct{}{}
		out = append(out, s)
		if len(out) == maxStates {
			break
		}
	}
	return out
}

func joinKey(toks []token) string {
	var b strings.Builder
	for i, t := range toks {
		if i > 0 {
			b.WriteByte(' ')
		}
		b.WriteString(t.canon)
	}
	return b.String()
}

func appendWord(words []wordRef, w wordRef) []wordRef {
	out := make([]wordRef, len(words)+1)
	copy(out, words)
	out[len(words)] = w
	return out
}

func commentOf(words []wordRef) string {
	var b strings.Builder
	for i, w := range words {
		if i > 0 {
			b.WriteByte(' ')
		}
		b.WriteString(w.key)
	}
	return b.String()
}

// costScale 把「词频越高越便宜」折算为整数代价，等价于对概率取负对数后求和：
// 求和使词数多于必要切分的路径代价更高，从而让整词优先于逐字拼接。整数运算
// 保证排序确定性。
const costScale = 24000

// userWeight 是每次上屏折算的词频增量。取值远高于常见静态词频，使用户选过的候选
// 一次之后即升到同键首位，这是用户对排序最直接的反馈。
const userWeight = 20000

// fallbackCost 是词典缺失片段的代价，远高于任何真实词条，保证原样字母总排在末位。
const fallbackCost = 1 << 24

// costOf 是单个词条的代价：词频与用户词频越高代价越低，下限为 1。
func (e *Engine) costOf(id uint32) int64 {
	freq := int64(e.d.Word(id).Freq) + e.userFreq(id)*userWeight
	if freq < 1 {
		freq = 1
	}
	return max(1, costScale/freq)
}

// collect 汇总全部切分路径的候选，按文本去重取最高分，再按分数降序、文本升序排列。
func (e *Engine) collect(letters string) []cand {
	if letters == "" {
		return nil
	}
	byText := make(map[string]cand, 16)
	for _, toks := range e.spellings(letters) {
		for _, c := range e.pathCandidates(toks) {
			if prev, ok := byText[c.text]; ok && prev.cost <= c.cost {
				continue
			}
			byText[c.text] = c
		}
	}
	// 简拼只做整串精确匹配，且输入不是任何音节前缀时才启用：这样 "nh" 命中 你好，
	// 而 "zh" 仍按 zha/zhan/zhang 的全拼前缀解释，不会冒出无关的整词。
	if _, ambiguous := e.spellPrefix[letters]; !ambiguous && len(letters) >= 2 {
		e.abbrevCandidates(letters, byText)
	}
	out := make([]cand, 0, len(byText))
	hasClean := false
	for _, c := range byText {
		if !c.broken {
			hasClean = true
			break
		}
	}
	for _, c := range byText {
		if hasClean && c.broken {
			continue
		}
		out = append(out, c)
	}
	out = gateWordCount(out)
	sort.Slice(out, func(i, j int) bool {
		if out[i].cost != out[j].cost {
			return out[i].cost < out[j].cost
		}
		return out[i].text < out[j].text
	})
	return out
}
