package main

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/luobochuanqi/silk-ime/internal/dict"
)

func TestBuildSubcommandProducesLoadableDictionary(t *testing.T) {
	dir := t.TempDir()
	src := filepath.Join(dir, "source.tsv")
	out := filepath.Join(dir, "core.bin")
	if err := os.WriteFile(src, []byte("你好\tni hao\t10\n中国\tzhong guo\t5\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := run([]string{"build", "-o", out, src}); err != nil {
		t.Fatalf("构建失败: %v", err)
	}
	data, err := os.ReadFile(out)
	if err != nil {
		t.Fatal(err)
	}
	d, err := dict.Open(data)
	if err != nil {
		t.Fatalf("构建产物无法加载: %v", err)
	}
	if ids := d.ExactWords("ni hao", 2); len(ids) != 1 || d.Word(ids[0]).Text != "你好" {
		t.Fatalf("构建产物查词失败: %+v", d.Word(0))
	}
}

func TestBuildSubcommandFailsOnBadInput(t *testing.T) {
	dir := t.TempDir()
	src := filepath.Join(dir, "source.tsv")
	if err := os.WriteFile(src, []byte("坏行\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := run([]string{"build", "-o", filepath.Join(dir, "core.bin"), src}); err == nil {
		t.Fatal("畸形源文件应返回错误")
	}
	if err := run([]string{"build"}); err == nil {
		t.Fatal("缺少参数应返回错误")
	}
	if err := run([]string{"unknown"}); err == nil {
		t.Fatal("未知子命令应返回错误")
	}
	if err := run(nil); err == nil {
		t.Fatal("缺少子命令应返回错误")
	}
}
