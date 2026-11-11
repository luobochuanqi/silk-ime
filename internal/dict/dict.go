// Package dict 定义核心词库的只读二进制格式与构建器。
//
// 文件布局固定为：24 字节文件头（magic、版本、四个段偏移）+ 音节表 + 词条表 +
// 前缀索引 + 倒排 id 表。加载器只读取传入字节、不做任何写入，因此调用方可以
// 直接把 mmap 映射交给 Open；所有偏移与长度在加载阶段校验，畸形输入返回错误
// 而非崩溃，保证 fuzz 下不 panic。
package dict

import (
	"encoding/binary"
	"errors"
	"fmt"
	"sort"
	"strings"
	"unicode/utf8"
)

const (
	magic      = "SLKD"
	version    = 1
	headerSize = 24
	// 单条词条最大文本/拼音长度，受头部 u8 长度字段限制。
	maxFieldLen = 255
)

// ErrFormat 表示输入字节不是可识别的词库格式。
var ErrFormat = errors.New("dict: 词库格式错误")

// Word 是词条在内存中的表示，字段内容由构建期输入决定。
type Word struct {
	Text      string
	Key       string // 规范全拼音节，空格分隔
	Freq      int32
	Syllables int
}

// prefixEntry 映射一个拼音键前缀到倒排 id 区间。
type prefixEntry struct {
	key     string
	offset  uint32
	entries uint32
}

// Dictionary 是加载后的只读词库。所有字段在 Open 后不再变化，可被多会话并发读取。
type Dictionary struct {
	syllables []string
	words     []Word
	prefix    []prefixEntry
	ids       []uint32
}

// Open 校验并加载词库字节。返回的 Dictionary 不持有传入切片的引用，调用方可以
// 在其返回后复用或释放 data。
func Open(data []byte) (*Dictionary, error) {
	if len(data) < headerSize {
		return nil, fmt.Errorf("%w: 长度 %d 不足文件头", ErrFormat, len(data))
	}
	if string(data[:4]) != magic {
		return nil, fmt.Errorf("%w: magic 不匹配", ErrFormat)
	}
	if v := binary.LittleEndian.Uint32(data[4:]); v != version {
		return nil, fmt.Errorf("%w: 不支持的版本 %d", ErrFormat, v)
	}
	off := [4]int{}
	for i := range off {
		off[i] = int(binary.LittleEndian.Uint32(data[8+4*i:]))
	}
	if off[0] < headerSize || off[0] > off[1] || off[1] > off[2] || off[2] > off[3] || off[3] > len(data) {
		return nil, fmt.Errorf("%w: 段偏移越界或顺序错误", ErrFormat)
	}

	d := &Dictionary{}
	if err := d.readSyllables(data[off[0]:off[1]]); err != nil {
		return nil, err
	}
	if err := d.readWords(data[off[1]:off[2]]); err != nil {
		return nil, err
	}
	if err := d.readPrefix(data[off[2]:off[3]]); err != nil {
		return nil, err
	}
	if err := d.readIDs(data[off[3]:]); err != nil {
		return nil, err
	}
	return d, nil
}

func (d *Dictionary) readSyllables(sec []byte) error {
	if len(sec) < 4 {
		return fmt.Errorf("%w: 音节表截断", ErrFormat)
	}
	n := uint64(binary.LittleEndian.Uint32(sec))
	pos := 4
	for i := uint64(0); i < n; i++ {
		if pos >= len(sec) {
			return fmt.Errorf("%w: 音节表条目 %d 越界", ErrFormat, i)
		}
		l := int(sec[pos])
		pos++
		if l == 0 || pos+l > len(sec) {
			return fmt.Errorf("%w: 音节表条目 %d 长度非法", ErrFormat, i)
		}
		s := string(sec[pos : pos+l])
		if !isASCIIWord(s) {
			return fmt.Errorf("%w: 音节 %q 非 ASCII 字母", ErrFormat, s)
		}
		d.syllables = append(d.syllables, s)
		pos += l
	}
	return nil
}

func (d *Dictionary) readWords(sec []byte) error {
	if len(sec) < 4 {
		return fmt.Errorf("%w: 词条表截断", ErrFormat)
	}
	n := uint64(binary.LittleEndian.Uint32(sec))
	if 4+4*n > uint64(len(sec)) {
		return fmt.Errorf("%w: 词条偏移表越界", ErrFormat)
	}
	d.words = make([]Word, n)
	for i := uint64(0); i < n; i++ {
		rel := uint64(binary.LittleEndian.Uint32(sec[4+4*i:]))
		if rel+8 > uint64(len(sec)) {
			return fmt.Errorf("%w: 词条 %d 偏移越界", ErrFormat, i)
		}
		textLen := uint64(sec[rel])
		keyLen := uint64(sec[rel+1])
		syl := binary.LittleEndian.Uint16(sec[rel+2:])
		freq := int32(binary.LittleEndian.Uint32(sec[rel+4:]))
		body := sec[rel+8:]
		if textLen+keyLen > uint64(len(body)) {
			return fmt.Errorf("%w: 词条 %d 正文越界", ErrFormat, i)
		}
		text := string(body[:textLen])
		key := string(body[textLen : textLen+keyLen])
		if !utf8.ValidString(text) || !utf8.ValidString(key) {
			return fmt.Errorf("%w: 词条 %d 不是合法 UTF-8", ErrFormat, i)
		}
		if !isKey(key) {
			return fmt.Errorf("%w: 词条 %d 拼音键非法", ErrFormat, i)
		}
		d.words[i] = Word{Text: text, Key: key, Freq: freq, Syllables: int(syl)}
	}
	return nil
}

func (d *Dictionary) readPrefix(sec []byte) error {
	if len(sec) < 4 {
		return fmt.Errorf("%w: 前缀索引截断", ErrFormat)
	}
	n := uint64(binary.LittleEndian.Uint32(sec))
	pos := uint64(4)
	d.prefix = make([]prefixEntry, n)
	for i := uint64(0); i < n; i++ {
		if pos >= uint64(len(sec)) {
			return fmt.Errorf("%w: 前缀条目 %d 越界", ErrFormat, i)
		}
		l := uint64(sec[pos])
		pos++
		if l == 0 || pos+l+8 > uint64(len(sec)) {
			return fmt.Errorf("%w: 前缀条目 %d 长度非法", ErrFormat, i)
		}
		key := string(sec[pos : pos+l])
		if !isKey(key) {
			return fmt.Errorf("%w: 前缀 %q 非法", ErrFormat, key)
		}
		pos += l
		off := binary.LittleEndian.Uint32(sec[pos:])
		cnt := binary.LittleEndian.Uint32(sec[pos+4:])
		pos += 8
		d.prefix[i] = prefixEntry{key: key, offset: off, entries: cnt}
	}
	return nil
}

func (d *Dictionary) readIDs(sec []byte) error {
	if len(sec)%4 != 0 {
		return fmt.Errorf("%w: 倒排 id 表长度不是 4 的倍数", ErrFormat)
	}
	d.ids = make([]uint32, len(sec)/4)
	for i := range d.ids {
		d.ids[i] = binary.LittleEndian.Uint32(sec[4*i:])
	}
	for _, p := range d.prefix {
		end := uint64(p.offset) + 4*uint64(p.entries)
		if end > uint64(len(sec)) {
			return fmt.Errorf("%w: 倒排区间越界", ErrFormat)
		}
		for _, id := range d.ids[p.offset/4 : p.offset/4+p.entries] {
			if uint64(id) >= uint64(len(d.words)) {
				return fmt.Errorf("%w: 倒排引用不存在的词条 %d", ErrFormat, id)
			}
		}
	}
	return nil
}

// Syllables 返回词库出现过的全部音节，按字典序排列。返回值只读。
func (d *Dictionary) Syllables() []string { return d.syllables }

// Len 返回词条数量。
func (d *Dictionary) Len() int { return len(d.words) }

// Word 返回指定 id 的词条。调用方须保证 id < Len。
func (d *Dictionary) Word(id uint32) Word { return d.words[id] }

// Lookup 返回拼音键恰为 key 的词条 id，倒排内已按词频降序、id 升序排列。
// 返回值只读，不得修改。
func (d *Dictionary) Lookup(key string) []uint32 {
	lo, hi := 0, len(d.prefix)
	for lo < hi {
		mid := int(uint(lo+hi) >> 1)
		if d.prefix[mid].key < key {
			lo = mid + 1
		} else {
			hi = mid
		}
	}
	if lo == len(d.prefix) || d.prefix[lo].key != key {
		return nil
	}
	p := d.prefix[lo]
	return d.ids[p.offset/4 : p.offset/4+p.entries]
}

// ExactWords 返回拼音键为 key 且音节数等于 syllables 的词条 id，即输入码完整
// 覆盖该词全部音节的全匹配结果。
func (d *Dictionary) ExactWords(key string, syllables int) []uint32 {
	all := d.Lookup(key)
	if len(all) == 0 {
		return nil
	}
	var out []uint32
	for _, id := range all {
		if d.words[id].Syllables == syllables {
			out = append(out, id)
		}
	}
	return out
}

func isASCIIWord(s string) bool {
	for i := range len(s) {
		if s[i] < 'a' || s[i] > 'z' {
			return false
		}
	}
	return s != ""
}

func isKey(s string) bool {
	if s == "" {
		return false
	}
	for i := range len(s) {
		if c := s[i]; c != ' ' && (c < 'a' || c > 'z') {
			return false
		}
	}
	return true
}

func sortedKeys[V any](m map[string]V) []string {
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	sort.Strings(out)
	return out
}

// PrefixLookup 返回拼音键以 key 开头且音节数等于 syllables 的词条 id，按前缀索引
// 顺序排列（同键内词频降序）。用于末尾未完成音节的候选检索。
func (d *Dictionary) PrefixLookup(key string, syllables int) []uint32 {
	lo, hi := 0, len(d.prefix)
	for lo < hi {
		mid := int(uint(lo+hi) >> 1)
		if d.prefix[mid].key < key {
			lo = mid + 1
		} else {
			hi = mid
		}
	}
	var out []uint32
	for ; lo < len(d.prefix) && strings.HasPrefix(d.prefix[lo].key, key); lo++ {
		p := d.prefix[lo]
		for _, id := range d.ids[p.offset/4 : p.offset/4+p.entries] {
			if d.words[id].Syllables == syllables {
				out = append(out, id)
			}
		}
	}
	return out
}
