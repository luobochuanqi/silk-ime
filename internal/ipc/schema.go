package ipc

import (
	"bytes"
	"encoding/json"
	"fmt"
)

// fieldKind 是载荷字段的 JSON 类型，用于在不解码具体结构的前提下校验语义。
type fieldKind uint8

const (
	kindString fieldKind = iota
	kindNumber
	kindBool
	kindObject
	kindArray
)

// fieldSpec 描述载荷字段的存在性要求与类型。schema 仅在 kindObject 或
// kindArray（元素为对象）时生效。
type fieldSpec struct {
	kind     fieldKind
	required bool
	schema   objectSchema
}

type objectSchema map[string]fieldSpec

// messageSpec 描述一种消息的约束：会话级消息必须带 sid。
type messageSpec struct {
	requireSID bool
	payload    objectSchema
}

var (
	candidateFields = objectSchema{
		"text":    {kind: kindString, required: true},
		"comment": {kind: kindString},
	}
	modeFields = objectSchema{
		"chinese":    {kind: kindBool, required: true},
		"full_punct": {kind: kindBool, required: true},
	}
	rectFields = objectSchema{
		"x": {kind: kindNumber, required: true},
		"y": {kind: kindNumber, required: true},
		"w": {kind: kindNumber, required: true},
		"h": {kind: kindNumber, required: true},
	}
)

// messages 是消息语义的唯一事实来源：类型未知即拒绝，缺失必填字段即拒绝。
var messages = map[string]messageSpec{
	TypeHello: {payload: objectSchema{
		"v":     {kind: kindNumber, required: true},
		"caps":  {kind: kindNumber, required: true},
		"build": {kind: kindString, required: true},
		"pid":   {kind: kindNumber},
	}},
	TypeWelcome: {payload: objectSchema{
		"v":   {kind: kindNumber, required: true},
		"cfg": {kind: kindObject, required: true},
	}},
	TypeFocusIn: {payload: objectSchema{
		"app":       {kind: kindString, required: true},
		"hwnd":      {kind: kindNumber, required: true},
		"lang_hint": {kind: kindString, required: true},
	}},
	TypeFocusOut: {requireSID: true},
	TypeKey: {requireSID: true, payload: objectSchema{
		"code": {kind: kindNumber, required: true},
		"rune": {kind: kindString},
		"down": {kind: kindBool, required: true},
		"mods": {kind: kindNumber, required: true},
		"ts":   {kind: kindNumber, required: true},
	}},
	TypeAction: {requireSID: true, payload: objectSchema{
		"kind":  {kind: kindString, required: true},
		"index": {kind: kindNumber},
		"mode":  {kind: kindObject, schema: modeFields},
	}},
	TypeUpdate: {requireSID: true, payload: objectSchema{
		"preedit":    {kind: kindString, required: true},
		"caret":      {kind: kindNumber, required: true},
		"candidates": {kind: kindArray, required: true, schema: candidateFields},
		"page_index": {kind: kindNumber, required: true},
		"mode":       {kind: kindObject, required: true, schema: modeFields},
	}},
	TypeCommit: {requireSID: true, payload: objectSchema{
		"text": {kind: kindString, required: true},
	}},
	TypeCaret: {requireSID: true, payload: objectSchema{
		"rect": {kind: kindObject, required: true, schema: rectFields},
	}},
	TypeShow: {requireSID: true, payload: objectSchema{
		"view": {kind: kindObject},
	}},
	TypeHide: {requireSID: true},
	TypePing: {payload: objectSchema{
		"ts": {kind: kindNumber, required: true},
	}},
	TypePong: {payload: objectSchema{
		"ts": {kind: kindNumber, required: true},
	}},
	TypeError: {payload: objectSchema{
		"id":   {kind: kindNumber, required: true},
		"code": {kind: kindNumber, required: true},
		"msg":  {kind: kindString, required: true},
	}},
	TypeBye: {requireSID: true},
}

func (e Envelope) validate() error {
	if e.V != Version {
		return fmt.Errorf("%w: 不支持的协议版本 %d", ErrInvalidMessage, e.V)
	}
	spec, ok := messages[e.Type]
	if !ok {
		return fmt.Errorf("%w: 未知消息类型 %q", ErrInvalidMessage, e.Type)
	}
	if spec.requireSID && e.SID == "" {
		return fmt.Errorf("%w: %s 缺少 sid", ErrInvalidMessage, e.Type)
	}
	if len(bytes.TrimSpace(e.Payload)) == 0 {
		return fmt.Errorf("%w: %s 缺少 payload", ErrInvalidMessage, e.Type)
	}
	if err := spec.payload.validate(e.Payload); err != nil {
		return fmt.Errorf("%w: %s: %v", ErrInvalidMessage, e.Type, err)
	}
	return nil
}

// validate 校验 raw 是 JSON 对象且满足 schema；schema 之外的字段被忽略。
func (s objectSchema) validate(raw json.RawMessage) error {
	trimmed := bytes.TrimSpace(raw)
	if len(trimmed) == 0 || trimmed[0] != '{' {
		return fmt.Errorf("载荷必须是 JSON 对象")
	}
	var obj map[string]json.RawMessage
	if err := json.Unmarshal(trimmed, &obj); err != nil {
		return fmt.Errorf("载荷解析失败: %v", err)
	}
	for name, spec := range s {
		value, present := obj[name]
		if !present {
			if spec.required {
				return fmt.Errorf("缺少字段 %q", name)
			}
			continue
		}
		if err := spec.validate(value); err != nil {
			return fmt.Errorf("字段 %q: %v", name, err)
		}
	}
	return nil
}

func (f fieldSpec) validate(raw json.RawMessage) error {
	kind, ok := classify(raw)
	if !ok || kind != f.kind {
		return fmt.Errorf("类型不匹配")
	}
	switch f.kind {
	case kindObject:
		return f.schema.validate(raw)
	case kindArray:
		var items []json.RawMessage
		if err := json.Unmarshal(bytes.TrimSpace(raw), &items); err != nil {
			return fmt.Errorf("数组解析失败: %v", err)
		}
		for i, item := range items {
			if err := f.schema.validate(item); err != nil {
				return fmt.Errorf("第 %d 项: %v", i, err)
			}
		}
	}
	return nil
}

func classify(raw json.RawMessage) (fieldKind, bool) {
	t := bytes.TrimSpace(raw)
	if len(t) == 0 {
		return 0, false
	}
	switch c := t[0]; {
	case c == '"':
		return kindString, true
	case c == 't' || c == 'f':
		return kindBool, true
	case c == '{':
		return kindObject, true
	case c == '[':
		return kindArray, true
	case c == '-' || (c >= '0' && c <= '9'):
		return kindNumber, true
	}
	return 0, false
}
