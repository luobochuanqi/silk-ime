// Package session 是引擎之上的会话运行时：按连接隔离会话、维护跨会话共享的输入模式，
// 并负责用户词频增量的落盘。
//
// 所有权：Manager 进程内唯一，跨连接共享；每条 shim 连接持有一个 Client，sid 只在
// Client 内唯一（shim 各实例独立编号，不同进程的 "s-1" 互不相干）。Client 由连接读循环
// 串行调用，无需加锁；连接断开时 Close 回收其全部会话。
package session

import (
	"errors"
	"sync"
	"sync/atomic"
	"time"

	"github.com/luobochuanqi/silk-ime/internal/engine"
)

// ErrUnknownSession 表示 sid 在本连接内没有对应会话（未 focus_in 或已结束）。
var ErrUnknownSession = errors.New("未知会话")

// DefaultMode 是 host 启动时的输入模式：中文、全角标点。
var DefaultMode = engine.Mode{Chinese: true, FullPunct: true}

// Manager 持有引擎与跨会话状态（输入模式、活动计数、待落盘词频）。并发安全。
type Manager struct {
	eng      *engine.Engine
	userPath string

	active  atomic.Int64
	lastUse atomic.Int64

	modeMu sync.Mutex
	mode   engine.Mode

	flushMu sync.Mutex
	// pending 是已从引擎取出但尚未成功落盘的增量；写失败时保留，下次重试。
	pending []engine.UserEntry
}

// NewManager 创建管理器；userPath 为空表示不落盘用户词频（测试用）。
func NewManager(eng *engine.Engine, userPath string) *Manager {
	m := &Manager{eng: eng, userPath: userPath, mode: DefaultMode}
	m.touch()
	return m
}

// ActiveSessions 返回所有连接上的活动会话总数（空闲自退出的判定依据）。
func (m *Manager) ActiveSessions() int {
	return int(m.active.Load())
}

// LastUse 返回最近一次会话活动的时间。
func (m *Manager) LastUse() time.Time {
	return time.Unix(0, m.lastUse.Load())
}

// Mode 返回当前输入模式。模式跨会话共享：在一个应用里切到英文，换到别的应用仍是英文。
func (m *Manager) Mode() engine.Mode {
	m.modeMu.Lock()
	defer m.modeMu.Unlock()
	return m.mode
}

func (m *Manager) setMode(mode engine.Mode) {
	m.modeMu.Lock()
	m.mode = mode
	m.modeMu.Unlock()
}

func (m *Manager) touch() {
	m.lastUse.Store(time.Now().UnixNano())
}

// NewClient 为一条 shim 连接创建会话命名空间。调用方必须在连接结束时调用 Close。
func (m *Manager) NewClient() *Client {
	return &Client{m: m, sessions: make(map[string]*engine.Session)}
}

// Flush 将引擎的用户词频增量合并进用户词库并原子落盘；无变更时为空操作。
// 写失败时增量留在内存，下次调用重试，不会丢失。
func (m *Manager) Flush() error {
	delta := m.eng.DrainUserDelta()
	if m.userPath == "" {
		return nil
	}
	m.flushMu.Lock()
	defer m.flushMu.Unlock()
	m.pending = append(m.pending, delta...)
	if len(m.pending) == 0 {
		return nil
	}
	entries, err := LoadUserEntries(m.userPath)
	if err == nil {
		err = SaveUserEntries(m.userPath, mergeEntries(entries, m.pending))
	}
	if err != nil {
		// 合并同词条，保证持续写失败时内存占用不随按键线性增长。
		m.pending = mergeEntries(nil, m.pending)
		return err
	}
	m.pending = nil
	return nil
}

// Client 是单条 shim 连接内的会话表。非并发安全，由连接读循环串行调用。
type Client struct {
	m        *Manager
	sessions map[string]*engine.Session
}

// FocusIn 为新文档创建会话并返回初始状态；同 sid 重复进入时以新文档为准重建。
// 新会话继承当前共享模式。
func (c *Client) FocusIn(sid, app string) engine.Response {
	c.m.touch()
	if old, ok := c.sessions[sid]; ok {
		old.Close()
	} else {
		c.m.active.Add(1)
	}
	s := c.m.eng.NewSession(engine.SessionMeta{App: app})
	c.sessions[sid] = s
	mode := c.m.Mode()
	return s.Handle(engine.Event{Toggle: &mode})
}

// FocusOut 结束会话并返回引擎的收尾响应（失焦上屏策略产生的 commit）。
func (c *Client) FocusOut(sid string) (engine.Response, error) {
	s, ok := c.remove(sid)
	if !ok {
		return engine.Response{}, ErrUnknownSession
	}
	resp := s.Handle(engine.Event{FocusOut: true})
	s.Close()
	return resp, nil
}

// End 静默结束会话（shim 的 bye），不产生响应；会话不存在时为空操作。
func (c *Client) End(sid string) {
	if s, ok := c.remove(sid); ok {
		s.Close()
	}
}

// Handle 把事件路由到会话，并把响应中的模式记为共享模式。
func (c *Client) Handle(sid string, ev engine.Event) (engine.Response, error) {
	c.m.touch()
	s, ok := c.sessions[sid]
	if !ok {
		return engine.Response{}, ErrUnknownSession
	}
	resp := s.Handle(ev)
	c.m.setMode(resp.Mode)
	return resp, nil
}

// Len 返回本连接的活动会话数。
func (c *Client) Len() int {
	return len(c.sessions)
}

// Close 结束本连接的全部会话。连接已断，收尾响应无处投递，直接丢弃。
func (c *Client) Close() {
	for sid := range c.sessions {
		c.End(sid)
	}
}

func (c *Client) remove(sid string) (*engine.Session, bool) {
	s, ok := c.sessions[sid]
	if ok {
		delete(c.sessions, sid)
		c.m.active.Add(-1)
	}
	return s, ok
}

// mergeEntries 把增量按（词, 拼音）合并进既有条目，返回合并后的列表。
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
