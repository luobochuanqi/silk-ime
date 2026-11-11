package ipc

import (
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"unicode/utf8"
)

// MaxFrameSize 是帧体的上限（1 MiB）。超出即视为攻击或流错位，接收方必须
// 拒绝并断开连接，不得按上限截断后继续解析。
const MaxFrameSize = 1 << 20

var (
	// ErrFrameTooLarge 表示声明的帧长超过 MaxFrameSize。
	ErrFrameTooLarge = errors.New("ipc: 帧长超过上限")
	// ErrInvalidMessage 表示封套或载荷不满足协议语义。
	ErrInvalidMessage = errors.New("ipc: 消息无效")
)

// WriteFrame 将一整帧（Encode 的产物）写入 w，整帧单次写入。
//
// 帧自带 u32 长度前缀；写前校验前缀完整性，不匹配视为编程错误。
// 整帧单次 Write：该约束与传输模式无关（消息模式按写分消息会劈开帧，
// 字节模式下也避免中间态）。w 的错误经 %w 包装后返回。
func WriteFrame(w io.Writer, frame []byte) error {
	if len(frame) < 4 {
		return fmt.Errorf("%w: 帧短于长度前缀", ErrInvalidMessage)
	}
	n := binary.LittleEndian.Uint32(frame)
	if n > MaxFrameSize {
		return fmt.Errorf("%w: %d 字节", ErrFrameTooLarge, n)
	}
	if int(n) != len(frame)-4 {
		return fmt.Errorf("%w: 长度前缀 %d 与实际 %d 不符", ErrInvalidMessage, n, len(frame)-4)
	}
	if _, err := w.Write(frame); err != nil {
		return fmt.Errorf("ipc: 写入帧: %w", err)
	}
	return nil
}

// ReadFrame 从 r 读取一帧并返回完整帧（含长度前缀），可直接交给 Decode。
//
// 返回的切片由调用方独占。长度前缀超限时返回 ErrFrameTooLarge；流在帧头前
// 结束返回 io.EOF，帧体不完整返回 io.ErrUnexpectedEOF。超限后流位置已不可
// 恢复，调用方必须断开。
func ReadFrame(r io.Reader) ([]byte, error) {
	var header [4]byte
	if _, err := io.ReadFull(r, header[:]); err != nil {
		return nil, fmt.Errorf("ipc: 读取帧头: %w", err)
	}
	n := binary.LittleEndian.Uint32(header[:])
	if n > MaxFrameSize {
		return nil, fmt.Errorf("%w: %d 字节", ErrFrameTooLarge, n)
	}
	frame := make([]byte, 4+n)
	copy(frame, header[:])
	if _, err := io.ReadFull(r, frame[4:]); err != nil {
		return nil, fmt.Errorf("ipc: 读取帧体: %w", err)
	}
	return frame, nil
}

// Encode 校验封套语义后返回整帧（长度前缀 + JSON）。
//
// 编码失败说明调用方构造了非法消息，属于编程错误而非输入问题。
func Encode(env Envelope) ([]byte, error) {
	if err := env.validate(); err != nil {
		return nil, err
	}
	body, err := json.Marshal(env)
	if err != nil {
		return nil, fmt.Errorf("%w: 序列化封套: %v", ErrInvalidMessage, err)
	}
	frame := make([]byte, 4+len(body))
	binary.LittleEndian.PutUint32(frame, uint32(len(body)))
	copy(frame[4:], body)
	return frame, nil
}

// Decode 解析整帧并做语义校验。
//
// 输入来自不可信进程：任何字节序列都只产生错误，不会 panic。
func Decode(frame []byte) (Envelope, error) {
	if len(frame) < 4 {
		return Envelope{}, fmt.Errorf("%w: 帧短于长度前缀", ErrInvalidMessage)
	}
	n := binary.LittleEndian.Uint32(frame)
	if n > MaxFrameSize {
		return Envelope{}, fmt.Errorf("%w: %d 字节", ErrFrameTooLarge, n)
	}
	body := frame[4:]
	if int(n) != len(body) {
		return Envelope{}, fmt.Errorf("%w: 长度前缀 %d 与实际 %d 不符", ErrInvalidMessage, n, len(body))
	}
	if !utf8.Valid(body) {
		return Envelope{}, fmt.Errorf("%w: 帧体不是合法 UTF-8", ErrInvalidMessage)
	}

	var wire wireEnvelope
	if err := json.Unmarshal(body, &wire); err != nil {
		return Envelope{}, fmt.Errorf("%w: 封套解析失败: %v", ErrInvalidMessage, err)
	}
	switch {
	case wire.V == nil:
		return Envelope{}, fmt.Errorf("%w: 缺少 v", ErrInvalidMessage)
	case wire.ID == nil:
		return Envelope{}, fmt.Errorf("%w: 缺少 id", ErrInvalidMessage)
	case wire.Type == nil:
		return Envelope{}, fmt.Errorf("%w: 缺少 type", ErrInvalidMessage)
	case wire.Payload == nil:
		return Envelope{}, fmt.Errorf("%w: 缺少 payload", ErrInvalidMessage)
	}

	env := Envelope{
		V:       *wire.V,
		ID:      *wire.ID,
		Type:    *wire.Type,
		SID:     wire.SID,
		Payload: wire.Payload,
	}
	if err := env.validate(); err != nil {
		return Envelope{}, err
	}
	return env, nil
}
