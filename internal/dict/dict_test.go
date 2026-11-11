package dict

import (
	"bytes"
	"os"
	"strings"
	"testing"
)

func loadSource(t *testing.T) []Source {
	t.Helper()
	data, err := os.ReadFile("../../dict/source.tsv")
	if err != nil {
		t.Fatalf("读取词库源文件: %v", err)
	}
	src, err := ParseSource(data)
	if err != nil {
		t.Fatalf("解析词库源文件: %v", err)
	}
	return src
}

func TestParseSourceRejectsMalformedLines(t *testing.T) {
	cases := map[string]string{
		"缺拼音":   "你好\n",
		"字段过多":  "你好\tni hao\t1\tx\n",
		"频次非法":  "你好\tni hao\tmany\n",
		"拼音非字母": "你好\tn1 hao\t1\n",
		"空词":    "\tni hao\t1\n",
	}
	for name, input := range cases {
		if _, err := ParseSource([]byte(input)); err == nil {
			t.Errorf("%s: 期望解析失败", name)
		}
	}
}

func TestParseSourceIgnoresCommentsAndDedupes(t *testing.T) {
	src, err := ParseSource([]byte("# 注释\n\n你好\tni hao\t10\n你好\tni hao\t20\n"))
	if err != nil {
		t.Fatal(err)
	}
	if len(src) != 1 || src[0].Frequency != 20 {
		t.Fatalf("期望保留最大频次的一条记录，实际 %+v", src)
	}
}

func TestBuildRoundTrip(t *testing.T) {
	src := loadSource(t)
	bin, err := Build(src)
	if err != nil {
		t.Fatal(err)
	}
	d, err := Open(bin)
	if err != nil {
		t.Fatal(err)
	}
	if d.Len() != len(src) {
		t.Fatalf("词条数 %d，期望 %d", d.Len(), len(src))
	}
	ids := d.ExactWords("ni hao", 2)
	if len(ids) == 0 {
		t.Fatal("全匹配检索为空")
	}
	if got := d.Word(ids[0]); got.Text != "你好" {
		t.Fatalf("首选词条为 %q，期望 你好", got.Text)
	}
	// 倒排内同键词条按词频降序。
	for i := 1; i < len(ids); i++ {
		prev, cur := d.Word(ids[i-1]), d.Word(ids[i])
		if prev.Freq < cur.Freq {
			t.Fatalf("倒排未按词频降序: %d < %d", prev.Freq, cur.Freq)
		}
	}
	if got := d.ExactWords("ni hao", 1); len(got) != 0 {
		t.Fatalf("音节数过滤失效: %v", got)
	}
}

func TestBuildIsDeterministic(t *testing.T) {
	src := loadSource(t)
	a, err := Build(src)
	if err != nil {
		t.Fatal(err)
	}
	b, err := Build(src)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(a, b) {
		t.Fatal("同一源数据两次构建结果不一致")
	}
}

func TestPrefixLookupMatchesKeyPrefix(t *testing.T) {
	d, err := Open(mustBuild(t, loadSource(t)))
	if err != nil {
		t.Fatal(err)
	}
	ids := d.PrefixLookup("zhong g", 2)
	if len(ids) == 0 {
		t.Fatal("前缀检索为空")
	}
	for _, id := range ids {
		w := d.Word(id)
		if !strings.HasPrefix(w.Key, "zhong g") || w.Syllables != 2 {
			t.Fatalf("前缀检索返回不符合条件的词条 %+v", w)
		}
	}
	if got := d.Lookup("no such key"); got != nil {
		t.Fatalf("不存在的键应返回 nil，得到 %v", got)
	}
}

func TestOpenRejectsCorruptedData(t *testing.T) {
	good := mustBuild(t, loadSource(t))
	cases := map[string][]byte{
		"空":      nil,
		"截断文件头":  good[:10],
		"magic错": append([]byte("XXXX"), good[4:]...),
		"版本不支持":  corrupt(good, 4, 99),
		"偏移逆序":   corrupt(good, 12, 1),
		"偏移越界":   corrupt(good, 20, 0xFFFF),
		"倒排长度非法": good[:len(good)-1],
		"音节数溢出":  corrupt(good, 24, 0xFFFFFF),
	}
	for name, data := range cases {
		if _, err := Open(data); err == nil {
			t.Errorf("%s: 期望加载失败", name)
		}
	}
}

func corrupt(data []byte, off int, v uint32) []byte {
	out := bytes.Clone(data)
	out[off] = byte(v)
	out[off+1] = byte(v >> 8)
	out[off+2] = byte(v >> 16)
	out[off+3] = byte(v >> 24)
	return out
}

func mustBuild(t *testing.T, src []Source) []byte {
	t.Helper()
	bin, err := Build(src)
	if err != nil {
		t.Fatal(err)
	}
	return bin
}
