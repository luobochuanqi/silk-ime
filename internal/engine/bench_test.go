package engine

import "testing"

// BenchmarkHandle 覆盖创建会话到整词上屏的完整序列，对应设计文档的一键到候选路径。
func BenchmarkHandle(b *testing.B) {
	e, err := Open(testDict(b), nil, Config{PageSize: 5})
	if err != nil {
		b.Fatal(err)
	}
	var keys [8]Key
	for i, r := range "zhongguo" {
		keys[i] = key(r)
	}
	b.ReportAllocs()
	b.ResetTimer()
	for range b.N {
		s := e.NewSession(SessionMeta{App: "bench"})
		for i := range keys {
			s.Handle(Event{Key: &keys[i]})
		}
		s.Close()
	}
}

// BenchmarkHandleKey 测量组合态下单键成本（含候选页重建），用于核对热路径预算。
func BenchmarkHandleKey(b *testing.B) {
	e, err := Open(testDict(b), nil, Config{PageSize: 5})
	if err != nil {
		b.Fatal(err)
	}
	s := e.NewSession(SessionMeta{App: "bench"})
	for _, r := range "zhong" {
		k := key(r)
		s.Handle(Event{Key: &k})
	}
	k := key('g')
	b.ReportAllocs()
	b.ResetTimer()
	for range b.N {
		s.Handle(Event{Key: &k})
	}
}
