// Command silkdict 构建核心词库二进制。
//
// 用法：silkdict build -o <out> <source.tsv>
package main

import (
	"errors"
	"flag"
	"fmt"
	"os"

	"github.com/luobochuanqi/silk-ime/internal/dict"
)

func main() {
	if err := run(os.Args[1:]); err != nil {
		fmt.Fprintln(os.Stderr, "silkdict:", err)
		os.Exit(1)
	}
}

func run(args []string) error {
	if len(args) == 0 {
		return errors.New("缺少子命令，可用：build")
	}
	switch args[0] {
	case "build":
		fs := flag.NewFlagSet("build", flag.ContinueOnError)
		out := fs.String("o", "", "输出文件路径")
		if err := fs.Parse(args[1:]); err != nil {
			return err
		}
		if *out == "" || fs.NArg() != 1 {
			return errors.New("用法：silkdict build -o <out> <source.tsv>")
		}
		return dict.BuildFile(fs.Arg(0), *out)
	default:
		return fmt.Errorf("未知子命令 %q，可用：build", args[0])
	}
}
