package session

import (
	"bufio"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"

	"github.com/luobochuanqi/silk-ime/internal/engine"
)

// LoadUserEntries 读取纯文本用户词库（词<TAB>拼音<TAB>频次<TAB>时间戳）。
// 文件不存在返回空表；格式损坏的行跳过不致命——用户数据可手改是产品契约（05 设计）。
func LoadUserEntries(path string) ([]engine.UserEntry, error) {
	f, err := os.Open(path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, fmt.Errorf("读取用户词库: %w", err)
	}
	defer f.Close()

	var entries []engine.UserEntry
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		line := strings.TrimRight(sc.Text(), "\r")
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		parts := strings.Split(line, "\t")
		if len(parts) != 4 {
			continue
		}
		freq, err := strconv.Atoi(parts[2])
		if err != nil {
			continue
		}
		ts, err := strconv.ParseInt(parts[3], 10, 64)
		if err != nil {
			continue
		}
		entries = append(entries, engine.UserEntry{Word: parts[0], Pinyin: parts[1], Freq: freq, TS: ts})
	}
	if err := sc.Err(); err != nil {
		return nil, fmt.Errorf("读取用户词库: %w", err)
	}
	return entries, nil
}

// SaveUserEntries 原子写用户词库（临时文件 + rename），按频次降序排列。
func SaveUserEntries(path string, entries []engine.UserEntry) error {
	sort.SliceStable(entries, func(i, j int) bool {
		if entries[i].Freq != entries[j].Freq {
			return entries[i].Freq > entries[j].Freq
		}
		return entries[i].Word < entries[j].Word
	})
	var b strings.Builder
	b.WriteString("# Silk 用户词库：词\t拼音\t频次\t时间戳（可直接编辑）\n")
	for _, e := range entries {
		fmt.Fprintf(&b, "%s\t%s\t%d\t%d\n", e.Word, e.Pinyin, e.Freq, e.TS)
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return fmt.Errorf("创建用户数据目录: %w", err)
	}
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, []byte(b.String()), 0o644); err != nil {
		return fmt.Errorf("写入用户词库: %w", err)
	}
	if err := os.Rename(tmp, path); err != nil {
		return fmt.Errorf("替换用户词库: %w", err)
	}
	return nil
}
