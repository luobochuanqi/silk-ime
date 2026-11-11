package session

import (
	"os"
	"path/filepath"
	"reflect"
	"testing"

	"github.com/luobochuanqi/silk-ime/internal/engine"
)

func TestUserEntriesRoundTrip(t *testing.T) {
	path := filepath.Join(t.TempDir(), "userdict.txt")
	want := []engine.UserEntry{
		{Word: "你好", Pinyin: "ni hao", Freq: 5, TS: 1730000000},
		{Word: "拟好", Pinyin: "ni hao", Freq: 2, TS: 1720000000},
	}
	if err := SaveUserEntries(path, want); err != nil {
		t.Fatalf("Save: %v", err)
	}
	got, err := LoadUserEntries(path)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("往返不一致:\n得 %v\n期 %v", got, want)
	}
}

func TestLoadUserEntriesSkipsBrokenLines(t *testing.T) {
	path := filepath.Join(t.TempDir(), "userdict.txt")
	body := "# 注释\n\n你好\tni hao\t5\t173\n坏行\t只有两列\n词\tpy\tNaN\t173\n词2\tpy\t1\txx\n"
	if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	got, err := LoadUserEntries(path)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	want := []engine.UserEntry{{Word: "你好", Pinyin: "ni hao", Freq: 5, TS: 173}}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("坏行应跳过: 得 %v", got)
	}
}

func TestMergeEntries(t *testing.T) {
	base := []engine.UserEntry{{Word: "你好", Pinyin: "ni hao", Freq: 5, TS: 100}}
	delta := []engine.UserEntry{
		{Word: "你好", Pinyin: "ni hao", Freq: 3, TS: 200},
		{Word: "拟好", Pinyin: "ni hao", Freq: 1, TS: 150},
	}
	got := mergeEntries(base, delta)
	want := []engine.UserEntry{
		{Word: "你好", Pinyin: "ni hao", Freq: 8, TS: 200},
		{Word: "拟好", Pinyin: "ni hao", Freq: 1, TS: 150},
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("合并结果错误: 得 %v\n期 %v", got, want)
	}
}

func TestSaveUserEntriesSortsByFreq(t *testing.T) {
	path := filepath.Join(t.TempDir(), "userdict.txt")
	entries := []engine.UserEntry{
		{Word: "低频", Pinyin: "di pin", Freq: 1, TS: 1},
		{Word: "高频", Pinyin: "gao pin", Freq: 9, TS: 2},
	}
	if err := SaveUserEntries(path, entries); err != nil {
		t.Fatal(err)
	}
	got, err := LoadUserEntries(path)
	if err != nil {
		t.Fatal(err)
	}
	if got[0].Word != "高频" {
		t.Fatalf("应按频次降序: %v", got)
	}
	if _, err := os.Stat(path + ".tmp"); !os.IsNotExist(err) {
		t.Fatalf("临时文件应已被 rename 消费")
	}
}
