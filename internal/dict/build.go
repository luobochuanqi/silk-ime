package dict

import (
	"encoding/binary"
	"errors"
	"fmt"
	"os"
	"slices"
	"strconv"
	"strings"
)

// Source 是词库源数据的一条记录。
type Source struct {
	Text      string
	Pinyin    []string // 规范音节，小写 ASCII
	Frequency int32
}

// ParseSource 解析 dict/source.tsv 内容：每行 `词<TAB>拼音<TAB>频次`，拼音为空格
// 分隔的音节；空行与 `#` 开头的注释行忽略，频次缺省为 0。重复的 (词, 拼音) 保留
// 最大频次。返回记录按拼音、词排序，保证构建结果可复现。
func ParseSource(data []byte) ([]Source, error) {
	byKey := map[string]Source{}
	for i, line := range strings.Split(string(data), "\n") {
		line = strings.TrimSpace(strings.TrimSuffix(line, "\r"))
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		fields := strings.Split(line, "\t")
		if len(fields) < 2 || len(fields) > 3 {
			return nil, fmt.Errorf("dict: 第 %d 行字段数应为 2 或 3，实为 %d", i+1, len(fields))
		}
		text := strings.TrimSpace(fields[0])
		if text == "" {
			return nil, fmt.Errorf("dict: 第 %d 行词为空", i+1)
		}
		syllables := strings.Fields(fields[1])
		if len(syllables) == 0 {
			return nil, fmt.Errorf("dict: 第 %d 行拼音为空", i+1)
		}
		freq := int32(0)
		if len(fields) == 3 {
			v, err := strconv.ParseInt(strings.TrimSpace(fields[2]), 10, 32)
			if err != nil {
				return nil, fmt.Errorf("dict: 第 %d 行频次非法: %w", i+1, err)
			}
			freq = int32(v)
		}
		for j := range syllables {
			syllables[j] = strings.ToLower(syllables[j])
		}
		key := strings.Join(syllables, " ")
		if !isKey(key) {
			return nil, fmt.Errorf("dict: 第 %d 行拼音 %q 含非 ASCII 字母", i+1, key)
		}
		s := Source{Text: text, Pinyin: syllables, Frequency: freq}
		if old, ok := byKey[key+"\x00"+text]; ok && old.Frequency >= freq {
			continue
		}
		byKey[key+"\x00"+text] = s
	}
	out := make([]Source, 0, len(byKey))
	for _, s := range byKey {
		out = append(out, s)
	}
	slices.SortFunc(out, func(a, b Source) int {
		if c := strings.Compare(strings.Join(a.Pinyin, " "), strings.Join(b.Pinyin, " ")); c != 0 {
			return c
		}
		return strings.Compare(a.Text, b.Text)
	})
	return out, nil
}

// Build 把源记录编译为二进制词库。同一份源数据总是产生相同字节。
func Build(src []Source) ([]byte, error) {
	if len(src) == 0 {
		return nil, errors.New("dict: 词库为空")
	}
	words := slices.Clone(src)
	slices.SortFunc(words, func(a, b Source) int {
		ka, kb := strings.Join(a.Pinyin, " "), strings.Join(b.Pinyin, " ")
		if c := strings.Compare(ka, kb); c != 0 {
			return c
		}
		return strings.Compare(a.Text, b.Text)
	})

	sylSet := map[string]struct{}{}
	for _, w := range words {
		for _, s := range w.Pinyin {
			sylSet[s] = struct{}{}
		}
	}
	syllables := sortedKeys(sylSet)

	var sylSec []byte
	sylSec = binary.LittleEndian.AppendUint32(sylSec, uint32(len(syllables)))
	for _, s := range syllables {
		if len(s) > maxFieldLen {
			return nil, fmt.Errorf("dict: 音节 %q 过长", s)
		}
		sylSec = append(sylSec, byte(len(s)))
		sylSec = append(sylSec, s...)
	}

	// 词条表先写偏移数组占位，正文依次追加，避免二次遍历。
	var wordSec []byte
	wordSec = binary.LittleEndian.AppendUint32(wordSec, uint32(len(words)))
	for range words {
		wordSec = binary.LittleEndian.AppendUint32(wordSec, 0)
	}
	for i, w := range words {
		key := strings.Join(w.Pinyin, " ")
		if len(w.Text) > maxFieldLen || len(key) > maxFieldLen {
			return nil, fmt.Errorf("dict: 词条 %q 超长", w.Text)
		}
		rel := len(wordSec)
		wordSec = append(wordSec, byte(len(w.Text)), byte(len(key)))
		wordSec = binary.LittleEndian.AppendUint16(wordSec, uint16(len(w.Pinyin)))
		wordSec = binary.LittleEndian.AppendUint32(wordSec, uint32(w.Frequency))
		wordSec = append(wordSec, w.Text...)
		wordSec = append(wordSec, key...)
		binary.LittleEndian.PutUint32(wordSec[4+4*i:], uint32(rel))
	}

	// 同一拼音键的词条归为一组，组内按词频降序、id 升序。
	type group struct{ ids []uint32 }
	groups := map[string]*group{}
	keys := make([]string, 0, len(words))
	for i, w := range words {
		key := strings.Join(w.Pinyin, " ")
		g, ok := groups[key]
		if !ok {
			g = &group{}
			groups[key] = g
			keys = append(keys, key)
		}
		g.ids = append(g.ids, uint32(i))
	}
	slices.Sort(keys)

	var prefixSec, idSec []byte
	prefixSec = binary.LittleEndian.AppendUint32(prefixSec, uint32(len(keys)))
	idOffset := uint32(0)
	for _, key := range keys {
		g := groups[key]
		slices.SortFunc(g.ids, func(a, b uint32) int {
			fa, fb := words[a].Frequency, words[b].Frequency
			if fa != fb {
				if fa > fb {
					return -1
				}
				return 1
			}
			return int(a) - int(b)
		})
		prefixSec = append(prefixSec, byte(len(key)))
		prefixSec = append(prefixSec, key...)
		prefixSec = binary.LittleEndian.AppendUint32(prefixSec, idOffset*4)
		prefixSec = binary.LittleEndian.AppendUint32(prefixSec, uint32(len(g.ids)))
		for _, id := range g.ids {
			idSec = binary.LittleEndian.AppendUint32(idSec, id)
		}
		idOffset += uint32(len(g.ids))
	}

	out := make([]byte, headerSize, headerSize+len(sylSec)+len(wordSec)+len(prefixSec)+len(idSec))
	copy(out, magic)
	binary.LittleEndian.PutUint32(out[4:], version)
	off := headerSize
	for i, sec := range [][]byte{sylSec, wordSec, prefixSec, idSec} {
		binary.LittleEndian.PutUint32(out[8+4*i:], uint32(off))
		off += len(sec)
		out = append(out, sec...)
	}
	return out, nil
}

// BuildFile 读取 TSV 源文件并写出二进制词库。
func BuildFile(sourcePath, outPath string) error {
	data, err := os.ReadFile(sourcePath)
	if err != nil {
		return fmt.Errorf("dict: 读取源文件: %w", err)
	}
	src, err := ParseSource(data)
	if err != nil {
		return err
	}
	bin, err := Build(src)
	if err != nil {
		return err
	}
	if err := os.WriteFile(outPath, bin, 0o644); err != nil {
		return fmt.Errorf("dict: 写出词库: %w", err)
	}
	return nil
}
