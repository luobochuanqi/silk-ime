package config

import (
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

func TestLoadMissingFileReturnsDefaults(t *testing.T) {
	cfg, err := Load(filepath.Join(t.TempDir(), "absent.toml"))
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	def := Default()
	if !reflect.DeepEqual(cfg, def) {
		t.Fatalf("缺文件应返回默认配置，得到 %+v", cfg)
	}
}

func TestLoadOverridesDefaults(t *testing.T) {
	path := filepath.Join(t.TempDir(), "silk.toml")
	body := "page_size = 9\nfuzzy_groups = [[\"z\", \"zh\"]]\nenglish_completion = true\nidle_exit_minutes = 30\n"
	if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	cfg, err := Load(path)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if cfg.PageSize != 9 || !cfg.EnglishCompletion || cfg.IdleExitMinutes != 30 {
		t.Fatalf("配置未生效: %+v", cfg)
	}
	if len(cfg.FuzzyGroups) != 1 || len(cfg.FuzzyGroups[0]) != 2 {
		t.Fatalf("模糊音组未生效: %+v", cfg.FuzzyGroups)
	}
}

func TestLoadRejectsInvalidValues(t *testing.T) {
	cases := map[string]string{
		"page_size 越界": "page_size = 2\n",
		"模糊音组不完整":      "fuzzy_groups = [[\"z\"]]\n",
		"空闲分钟非正":       "idle_exit_minutes = 0\n",
		"非法 TOML":      "page_size =\n",
	}
	for name, body := range cases {
		path := filepath.Join(t.TempDir(), "silk.toml")
		if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
		if _, err := Load(path); err == nil {
			t.Errorf("%s: 应返回错误", name)
		}
	}
}
