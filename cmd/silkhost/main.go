// silkhost 是 Silk IME 的宿主进程：持有输入引擎、维护会话，经命名管道服务 TSF 适配层。
//
// 生命周期（ADR-5）：由 shim 按需拉起；无会话空闲超过配置阈值后自行退出。
package main

import (
	"errors"
	"flag"
	"log"
	"net"
	"os"
	"os/signal"
	"sync/atomic"
	"syscall"
	"time"

	"github.com/Microsoft/go-winio"
	"golang.org/x/sys/windows"

	"github.com/luobochuanqi/silk-ime/internal/config"
	"github.com/luobochuanqi/silk-ime/internal/engine"
	"github.com/luobochuanqi/silk-ime/internal/session"
)

func main() {
	var (
		cfgPath  = flag.String("config", "", "配置文件路径（默认 %LOCALAPPDATA%\\Silk\\silk.toml）")
		dictPath = flag.String("dict", "dict/silk.dict", "词库文件路径")
		pipeName = flag.String("pipe", "", "管道名（默认 \\\\.\\pipe\\silk-ime-<SID>）")
	)
	flag.Parse()
	log.SetPrefix("silkhost ")

	paths, err := config.DefaultPaths()
	if err != nil {
		log.Fatalf("数据目录: %v", err)
	}
	if *cfgPath != "" {
		paths.Config = *cfgPath
	}
	cfg, err := config.Load(paths.Config)
	if err != nil {
		log.Fatalf("配置: %v", err)
	}

	dictData, err := os.ReadFile(*dictPath)
	if err != nil {
		log.Fatalf("词库 %s: %v", *dictPath, err)
	}
	userEntries, err := session.LoadUserEntries(paths.UserDict)
	if err != nil {
		log.Fatalf("用户词库: %v", err)
	}
	eng, err := engine.Open(dictData, userEntries, engine.Config{
		PageSize:          cfg.PageSize,
		FuzzyGroups:       cfg.FuzzyGroups,
		EnglishCompletion: cfg.EnglishCompletion,
	})
	if err != nil {
		log.Fatalf("引擎: %v", err)
	}
	mgr := session.NewManager(eng, paths.UserDict)

	sid, err := currentUserSID()
	if err != nil {
		log.Fatalf("当前用户 SID: %v", err)
	}
	name := *pipeName
	if name == "" {
		name = `\\.\pipe\silk-ime-` + sid
	}
	ln, err := winio.ListenPipe(name, &winio.PipeConfig{
		SecurityDescriptor: "D:P(A;;GA;;;" + sid + ")",
		MessageMode:        true,
		InputBufferSize:    64 << 10,
		OutputBufferSize:   64 << 10,
	})
	if err != nil {
		// 管道被占用 = 已有实例在运行（ADR-5 单实例保证），静默退出。
		if errors.Is(err, windows.ERROR_PIPE_BUSY) {
			log.Print("已有实例运行，退出")
			return
		}
		log.Fatalf("监听管道 %s: %v", name, err)
	}
	log.Printf("就绪：%s", name)

	var (
		lastActive atomic.Int64
		activeConn atomic.Int32
	)
	lastActive.Store(time.Now().UnixNano())

	shutdown := make(chan struct{})
	go func() {
		sig := make(chan os.Signal, 1)
		signal.Notify(sig, os.Interrupt, syscall.SIGTERM)
		<-sig
		close(shutdown)
		ln.Close()
	}()

	go func() {
		for {
			nc, err := ln.Accept()
			if err != nil {
				return
			}
			activeConn.Add(1)
			lastActive.Store(time.Now().UnixNano())
			go func(nc net.Conn) {
				defer activeConn.Add(-1)
				c := &conn{nc: nc, mgr: mgr, snapshot: cfgSnapshot{
					PageSize:  cfg.PageSize,
					Chinese:   true,
					FullPunct: true,
				}}
				c.run()
				lastActive.Store(time.Now().UnixNano())
			}(nc)
		}
	}()

	idle := time.Duration(cfg.IdleExitMinutes) * time.Minute
	tick := time.NewTicker(30 * time.Second)
	defer tick.Stop()
	for {
		select {
		case <-shutdown:
			shutdownHost(mgr)
			return
		case <-tick.C:
			if activeConn.Load() == 0 && time.Since(time.Unix(0, lastActive.Load())) > idle {
				log.Print("空闲超时，退出")
				shutdownHost(mgr)
				return
			}
		}
	}
}

func shutdownHost(mgr *session.Manager) {
	if err := mgr.Close(); err != nil {
		log.Printf("落盘用户词库: %v", err)
	}
}

func currentUserSID() (string, error) {
	token, err := windows.OpenCurrentProcessToken()
	if err != nil {
		return "", err
	}
	defer token.Close()
	user, err := token.GetTokenUser()
	if err != nil {
		return "", err
	}
	return user.User.Sid.String(), nil
}
