package dict

import (
	"os"
	"testing"
)

// FuzzOpen 断言任意字节输入都不会让加载器 panic，只以错误返回；成功加载的
// 词库也必须在检索路径上保持有界。
func FuzzOpen(f *testing.F) {
	source, err := os.ReadFile("../../dict/source.tsv")
	if err != nil {
		f.Fatalf("读取词库源文件: %v", err)
	}
	src, err := ParseSource(source)
	if err != nil {
		f.Fatalf("解析词库源文件: %v", err)
	}
	bin, err := Build(src)
	if err != nil {
		f.Fatalf("构建种子词库: %v", err)
	}
	f.Add(bin)
	f.Add(bin[:headerSize+1])
	f.Add([]byte("SLKD"))
	f.Fuzz(func(t *testing.T, data []byte) {
		d, err := Open(data)
		if err != nil {
			return
		}
		_ = d.ExactWords("ni", 1)
		_ = d.PrefixLookup("n", 1)
		if d.Len() > 0 {
			_ = d.Word(0)
		}
	})
}
