// Package session 管理输入会话：焦点进出、事件路由与用户词频落盘。
//
// 会话状态的所有权在 Manager：每个会话由单一 goroutine（连接读循环）串行调用，
// 会话内互斥锁用于隔离未来多文档并发；引擎本身无共享可变状态（见 05 设计）。
package session

import (
	"fmt"
	"sync"

	"github.com/luobochuanqi/silk-ime/internal/engine"
)

// Manager 维护 sid → 会话 的路由，并负责用户词频增量的定期落盘。
type Manager struct {
	eng      *engine.Engine
	userPath string

	mu       sync.Mutex
	sessions map[string]*session
}

type session struct {
	mu sync.Mutex
	s  *engine.Session
}

// NewManager 创建管理器；userPath 为空表示不落盘用户词频（测试用）。
func NewManager(eng *engine.Engine, userPath string) *Manager {
	return &Manager{eng: eng, userPath: userPath, sessions: make(map[string]*session)}
}

// FocusIn 为新文档创建会话；同 sid 重复进入时重建（以新文档状态为准）。
func (m *Manager) FocusIn(sid, app string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.sessions[sid] = &session{s: m.eng.NewSession(engine.SessionMeta{App: app})}
}

// FocusOut 结束会话并返回引擎的收尾响应（失焦上屏策略产生的 commit）。
func (m *Manager) FocusOut(sid string) (engine.Response, error) {
	m.mu.Lock()
	s, ok := m.sessions[sid]
	if ok {
		delete(m.sessions, sid)
	}
	m.mu.Unlock()
	if !ok {
		return engine.Response{}, fmt.Errorf("未知会话 %s", sid)
	}
	resp := s.dispatch(engine.Event{FocusOut: true})
	s.s.Close()
	return resp, nil
}

// HandleKey 路由按键事件。
func (m *Manager) HandleKey(sid string, k engine.Key) (engine.Response, error) {
	return m.dispatch(sid, engine.Event{Key: &k})
}

// Select 路由候选选择（0 起）。
func (m *Manager) Select(sid string, index int) (engine.Response, error) {
	return m.dispatch(sid, engine.Event{Select: &index})
}

// Page 路由翻页（±1）。
func (m *Manager) Page(sid string, delta int) (engine.Response, error) {
	return m.dispatch(sid, engine.Event{Page: &delta})
}

// Toggle 路由模式切换。
func (m *Manager) Toggle(sid string, mode engine.Mode) (engine.Response, error) {
	return m.dispatch(sid, engine.Event{Toggle: &mode})
}

func (m *Manager) dispatch(sid string, ev engine.Event) (engine.Response, error) {
	m.mu.Lock()
	s, ok := m.sessions[sid]
	m.mu.Unlock()
	if !ok {
		return engine.Response{}, fmt.Errorf("未知会话 %s", sid)
	}
	return s.dispatch(ev), nil
}

// Flush 将引擎的用户词频增量合并落盘（原子写）。无变更时为空操作。
func (m *Manager) Flush() error {
	if m.userPath == "" {
		return nil
	}
	delta := m.eng.DrainUserDelta()
	m.mu.Lock()
	defer m.mu.Unlock()
	if len(delta) == 0 {
		return nil
	}
	entries, err := LoadUserEntries(m.userPath)
	if err != nil {
		return err
	}
	merged := mergeEntries(entries, delta)
	return SaveUserEntries(m.userPath, merged)
}

// Close 结束所有会话并落盘用户词频。
func (m *Manager) Close() error {
	m.mu.Lock()
	sessions := m.sessions
	m.sessions = make(map[string]*session)
	m.mu.Unlock()
	for _, s := range sessions {
		s.dispatch(engine.Event{FocusOut: true})
		s.s.Close()
	}
	return m.Flush()
}

func (s *session) dispatch(ev engine.Event) engine.Response {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.s.Handle(ev)
}

// mergeEntries 把增量按（词, 拼音）合并进既有条目，返回按词频降序的新列表。
func mergeEntries(entries []engine.UserEntry, delta []engine.UserEntry) []engine.UserEntry {
	index := make(map[[2]string]int, len(entries))
	for i, e := range entries {
		index[[2]string{e.Word, e.Pinyin}] = i
	}
	for _, d := range delta {
		if i, ok := index[[2]string{d.Word, d.Pinyin}]; ok {
			entries[i].Freq += d.Freq
			if d.TS > entries[i].TS {
				entries[i].TS = d.TS
			}
			continue
		}
		index[[2]string{d.Word, d.Pinyin}] = len(entries)
		entries = append(entries, d)
	}
	return entries
}
