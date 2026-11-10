// Package config 读取用户配置并定义数据目录布局。
package config

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"

	"github.com/BurntSushi/toml"
)

// Config 是 silk.toml 的全部配置项。
type Config struct {
	PageSize          int        `toml:"page_size"`
	FuzzyGroups       [][]string `toml:"fuzzy_groups"`
	EnglishCompletion bool       `toml:"english_completion"`
	IdleExitMinutes   int        `toml:"idle_exit_minutes"`
}

// Paths 是用户数据目录布局，整个目录可复制迁移（产品契约，见 05 设计）。
type Paths struct {
	Dir      string
	Config   string
	UserDict string
	Logs     string
}

// Default 返回默认配置。模糊音默认关闭：属于个人习惯，不做默认值立场（见 01 设计）。
func Default() Config {
	return Config{PageSize: 5, IdleExitMinutes: 15}
}

// DefaultPaths 返回 %LOCALAPPDATA%\Silk 下的目录布局。
func DefaultPaths() (Paths, error) {
	base := os.Getenv("LOCALAPPDATA")
	if base == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return Paths{}, fmt.Errorf("定位用户数据目录: %w", err)
		}
		base = filepath.Join(home, "AppData", "Local")
	}
	dir := filepath.Join(base, "Silk")
	return Paths{
		Dir:      dir,
		Config:   filepath.Join(dir, "silk.toml"),
		UserDict: filepath.Join(dir, "userdict.txt"),
		Logs:     filepath.Join(dir, "logs"),
	}, nil
}

// Load 读取配置文件；文件不存在返回默认配置。解析失败或非法值返回错误。
func Load(path string) (Config, error) {
	cfg := Default()
	data, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return cfg, nil
	}
	if err != nil {
		return cfg, fmt.Errorf("读取配置: %w", err)
	}
	if err := toml.Unmarshal(data, &cfg); err != nil {
		return cfg, fmt.Errorf("解析配置 %s: %w", path, err)
	}
	if err := cfg.validate(); err != nil {
		return cfg, err
	}
	return cfg, nil
}

func (c Config) validate() error {
	if c.PageSize < 3 || c.PageSize > 9 {
		return fmt.Errorf("page_size=%d 超出范围 [3,9]", c.PageSize)
	}
	for _, g := range c.FuzzyGroups {
		if len(g) < 2 {
			return fmt.Errorf("模糊音组 %v 至少需要两个等价拼写", g)
		}
	}
	if c.IdleExitMinutes < 1 {
		return fmt.Errorf("idle_exit_minutes=%d 必须为正", c.IdleExitMinutes)
	}
	return nil
}
