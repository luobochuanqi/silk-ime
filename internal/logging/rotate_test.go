package logging

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestRotatingFileKeepsNewestBackups(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "silkhost.log")
	w, err := openRotating(path, 10, 2)
	if err != nil {
		t.Fatal(err)
	}
	for _, line := range []string{"aaaaaaaa\n", "bbbbbbbb\n", "cccccccc\n", "dddddddd\n"} {
		if _, err := w.Write([]byte(line)); err != nil {
			t.Fatal(err)
		}
	}
	if err := w.Close(); err != nil {
		t.Fatal(err)
	}

	want := map[string]string{
		"silkhost.log":   "dddddddd\n",
		"silkhost.1.log": "cccccccc\n",
		"silkhost.2.log": "bbbbbbbb\n",
	}
	for name, content := range want {
		got, err := os.ReadFile(filepath.Join(dir, name))
		if err != nil || string(got) != content {
			t.Errorf("%s = %q (%v)，期望 %q", name, got, err, content)
		}
	}
	entries, _ := os.ReadDir(dir)
	for _, e := range entries {
		if strings.Contains(e.Name(), ".3.") {
			t.Errorf("超出保留数的备份未被淘汰: %s", e.Name())
		}
	}
}
