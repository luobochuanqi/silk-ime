// silkhost 是 Silk IME 的宿主进程：持有输入引擎、维护会话，经命名管道服务 TSF 适配层。
//
// 生命周期（ADR-5）：由 shim 按需拉起；无会话空闲超过配置阈值后自行退出。
package main

import (
	"errors"
	"flag"
	"fmt"
	"log/slog"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"runtime/debug"
	"sync/atomic"
	"syscall"
	"time"

	"github.com/Microsoft/go-winio"
	"golang.org/x/sys/windows"

	"github.com/luobochuanqi/silk-ime/internal/config"
	"github.com/luobochuanqi/silk-ime/internal/engine"
	"github.com/luobochuanqi/silk-ime/internal/logging"
	"github.com/luobochuanqi/silk-ime/internal/session"
)

// maintenanceInterval 同时决定用户词频落盘周期（崩溃最多丢这么久的增量）与空闲检查粒度。
const maintenanceInterval = 30 * time.Second

func main() {
	if err := run(); err != nil {
		slog.Error("silkhost 异常退出", "err", err)
		os.Exit(1)
	}
}

func run() error {
	var (
		cfgPath  = flag.String("config", "", "配置文件路径（默认 %LOCALAPPDATA%\\Silk\\silk.toml）")
		dictPath = flag.String("dict", "dict/silk.dict", "词库文件路径")
		pipeName = flag.String("pipe", "", "管道名（默认 \\\\.\\pipe\\silk-ime-<SID>）")
	)
	flag.Parse()

	paths, err := config.DefaultPaths()
	if err != nil {
		return err
	}
	if *cfgPath != "" {
		paths.Config = *cfgPath
	}
	// 配置损坏不阻止启动：host 起不来等于输入法整体失效，按默认值降级并告警。
	cfg, cfgErr := config.Load(paths.Config)

	_, logFile, err := logging.Setup(logging.Options{
		Path:     filepath.Join(paths.Logs, "silkhost.log"),
		Level:    cfg.LogLevel,
		MaxBytes: 2 << 20,
		Backups:  4,
		Console:  stderrAttached(),
	})
	if err != nil {
		slog.Warn("日志文件不可用，仅输出到 stderr", "err", err)
	} else {
		defer logFile.Close()
	}
	slog.Info("silkhost 启动", "version", buildVersion(), "pid", os.Getpid(), "data", paths.Dir)
	if cfgErr != nil {
		slog.Error("配置无效，使用默认配置", "path", paths.Config, "err", cfgErr)
	}
	declareDPIAwareness()

	dictData, err := os.ReadFile(*dictPath)
	if err != nil {
		return fmt.Errorf("读取词库: %w", err)
	}
	userEntries, err := session.LoadUserEntries(paths.UserDict)
	if err != nil {
		// 落盘时会再次读取并失败，增量留在内存，不会覆盖读不出来的用户词库。
		slog.Error("用户词库不可读，本次不加载", "path", paths.UserDict, "err", err)
	}
	eng, err := engine.Open(dictData, userEntries, engine.Config{
		PageSize:          cfg.PageSize,
		FuzzyGroups:       cfg.FuzzyGroups,
		EnglishCompletion: cfg.EnglishCompletion,
	})
	if err != nil {
		return fmt.Errorf("打开引擎: %w", err)
	}
	slog.Info("引擎就绪", "dict", *dictPath, "user_entries", len(userEntries), "page_size", cfg.PageSize)
	mgr := session.NewManager(eng, paths.UserDict)

	cw, err := loadCandidateWindow()
	if err != nil {
		slog.Warn("候选窗不可用，以无窗模式运行", "err", err)
	}
	ui := &presenter{w: cw}

	ln, err := listen(*pipeName)
	if errors.Is(err, errAlreadyRunning) {
		slog.Info("已有实例运行，退出")
		return nil
	}
	if err != nil {
		return err
	}

	shutdown := make(chan os.Signal, 1)
	signal.Notify(shutdown, os.Interrupt, syscall.SIGTERM)
	go serve(ln, mgr, ui, cfg.PageSize)

	defer flush(mgr)
	idle := time.Duration(cfg.IdleExitMinutes) * time.Minute
	tick := time.NewTicker(maintenanceInterval)
	defer tick.Stop()
	for {
		select {
		case sig := <-shutdown:
			slog.Info("收到退出信号", "signal", sig)
			ln.Close()
			return nil
		case <-tick.C:
			flush(mgr)
			// 无会话空闲超过阈值退出（连接常驻不算活动）。
			if mgr.ActiveSessions() == 0 && time.Since(mgr.LastUse()) > idle {
				slog.Info("空闲超时，退出", "idle", idle)
				ln.Close()
				return nil
			}
		}
	}
}

var errAlreadyRunning = errors.New("已有实例运行")

// listen 创建当前用户的协议管道；管道已存在即说明已有实例（ADR-5 单实例保证）。
func listen(name string) (net.Listener, error) {
	sid, err := currentUserSID()
	if err != nil {
		return nil, fmt.Errorf("当前用户 SID: %w", err)
	}
	if name == "" {
		name = `\\.\pipe\silk-ime-` + sid
	}
	ln, err := winio.ListenPipe(name, &winio.PipeConfig{
		// S:(ML;;NW;;;ME) 把管道的强制完整性定为中级：host 可能被高完整性宿主里的
		// shim 拉起，默认的高标签会让普通权限的 shim/后续实例一律 Access denied（O-4）。
		SecurityDescriptor: "D:P(A;;GA;;;" + sid + ")S:(ML;;NW;;;ME)",
		// 字节模式：帧由长度前缀自定界；消息模式按写分消息会劈开帧边界
		MessageMode:      false,
		InputBufferSize:  64 << 10,
		OutputBufferSize: 64 << 10,
	})
	if err != nil {
		// 首实例以 FILE_CREATE 创建管道：同名已存在时报 ALREADY_EXISTS（或拒绝访问）。
		if errors.Is(err, windows.ERROR_ALREADY_EXISTS) || errors.Is(err, windows.ERROR_PIPE_BUSY) ||
			errors.Is(err, windows.ERROR_ACCESS_DENIED) {
			return nil, errAlreadyRunning
		}
		return nil, fmt.Errorf("监听管道 %s: %w", name, err)
	}
	slog.Info("管道就绪", "pipe", name)
	return ln, nil
}

// serve 接受 shim 连接直到监听器关闭，每条连接一个读循环。
func serve(ln net.Listener, mgr *session.Manager, ui *presenter, pageSize int) {
	var ids atomic.Uint64
	for {
		nc, err := ln.Accept()
		if err != nil {
			if !errors.Is(err, winio.ErrPipeListenerClosed) {
				slog.Error("接受连接失败，停止服务", "err", err)
			}
			return
		}
		go newConn(ids.Add(1), nc, mgr, ui, pageSize).run()
	}
}

func flush(mgr *session.Manager) {
	if err := mgr.Flush(); err != nil {
		slog.Error("用户词库落盘失败，增量保留待重试", "err", err)
	}
}

func buildVersion() string {
	if info, ok := debug.ReadBuildInfo(); ok {
		return info.Main.Version
	}
	return "unknown"
}

// stderrAttached 判断进程是否有可用的 stderr（前台调试运行）。shim 以 CREATE_NO_WINDOW
// 拉起时标准句柄为空。
func stderrAttached() bool {
	_, err := os.Stderr.Stat()
	return err == nil
}

// declareDPIAwareness 声明 per-monitor DPI aware v2（06:50：host 进程声明，
// shim 内渲染按宿主进程 DPI 等级工作）；旧系统回退系统级 DPI 感知。
func declareDPIAwareness() {
	user32 := windows.NewLazySystemDLL("user32.dll")
	setCtx := user32.NewProc("SetProcessDpiAwarenessContext")
	// DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 = (HANDLE)-4
	if setCtx.Find() == nil {
		if r, _, _ := setCtx.Call(^uintptr(3)); r != 0 {
			return
		}
	}
	user32.NewProc("SetProcessDPIAware").Call()
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
