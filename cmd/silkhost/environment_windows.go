package main

import (
	"context"
	"debug/pe"
	"fmt"
	"log/slog"
	"os"
	"runtime"

	"golang.org/x/sys/windows"
)

func logStartup(dataDir string) {
	ctx := context.Background()
	if !slog.Default().Enabled(ctx, slog.LevelInfo) {
		return
	}
	// RtlGetVersion 不依赖应用清单中的受支持系统声明，避免把新版 Windows 误报为旧版。
	version := windows.RtlGetVersion()
	slog.LogAttrs(ctx, slog.LevelInfo, "silkhost 启动",
		slog.String("version", buildVersion()),
		slog.Int("pid", os.Getpid()),
		slog.String("data", dataDir),
		slog.String("windows_version", fmt.Sprintf("%d.%d", version.MajorVersion, version.MinorVersion)),
		slog.Uint64("windows_build", uint64(version.BuildNumber)),
		slog.String("process_arch", runtime.GOARCH),
		nativeArchitecture(),
		slog.String("go_version", runtime.Version()),
		slog.Int("cpu_logical", runtime.NumCPU()),
	)
}

func nativeArchitecture() slog.Attr {
	var processMachine, nativeMachine uint16
	// GOARCH 只代表当前进程；在 ARM64 模拟运行时必须向系统查询本机架构。
	if err := windows.IsWow64Process2(windows.CurrentProcess(), &processMachine, &nativeMachine); err != nil {
		return slog.Any("native_arch_err", err)
	}
	switch nativeMachine {
	case pe.IMAGE_FILE_MACHINE_I386:
		return slog.String("native_arch", "386")
	case pe.IMAGE_FILE_MACHINE_AMD64:
		return slog.String("native_arch", "amd64")
	case pe.IMAGE_FILE_MACHINE_ARMNT:
		return slog.String("native_arch", "arm")
	case pe.IMAGE_FILE_MACHINE_ARM64:
		return slog.String("native_arch", "arm64")
	default:
		// 保留系统返回的机器类型，不把尚未识别的架构猜成进程架构。
		return slog.Uint64("native_machine", uint64(nativeMachine))
	}
}
