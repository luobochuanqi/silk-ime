package engine

import (
	"reflect"
	"testing"
)

// events 是一段固定事件序列，覆盖组字、翻页、上屏、标点与模式切换。
func events() []Event {
	var evs []Event
	add := func(letters string) {
		for _, r := range letters {
			k := Key{Code: keyCodeForRune(r), Rune: r, Down: true, TS: 1}
			evs = append(evs, Event{Key: &k})
		}
	}
	add("zhongguo")
	down := Key{Code: KeyPageDown, Down: true, TS: 2}
	evs = append(evs, Event{Key: &down})
	idx := 1
	evs = append(evs, Event{Select: &idx})
	add("shi")
	up := Key{Code: KeyBackspace, Down: true, TS: 3}
	evs = append(evs, Event{Key: &up})
	add("jie")
	sp := Key{Code: KeySpace, Down: true, TS: 4}
	evs = append(evs, Event{Key: &sp})
	m := Mode{Chinese: true, FullPunct: true}
	evs = append(evs, Event{Toggle: &m})
	pr := Key{Code: KeyPunct, Rune: ',', Down: true, TS: 5}
	evs = append(evs, Event{Key: &pr})
	return evs
}

func replay(t *testing.T, e *Engine, evs []Event) []Response {
	t.Helper()
	s := e.NewSession(SessionMeta{App: "determinism"})
	defer s.Close()
	out := make([]Response, len(evs))
	for i := range evs {
		out[i] = s.Handle(evs[i])
	}
	return out
}

// TestHandleIsDeterministic 断言同一事件序列在两个全新会话上产生逐字段相同的响应。
// 引擎内部若混入时钟、随机或跨会话状态，这里会立即失败。
func TestHandleIsDeterministic(t *testing.T) {
	cfg := Config{PageSize: 5}
	first := replay(t, newTestEngine(t, cfg), events())
	second := replay(t, newTestEngine(t, cfg), events())
	if !reflect.DeepEqual(first, second) {
		t.Fatalf("两次重放响应不一致:\n%+v\n%+v", first, second)
	}
}

// TestReplayAfterUserDeltaStable 断言用户词频变化只影响排序，不改变响应结构。
func TestReplayAfterUserDeltaStable(t *testing.T) {
	evs := events()
	before := replay(t, newTestEngine(t, Config{PageSize: 5}), evs)
	e := newTestEngine(t, Config{PageSize: 5})
	after := replay(t, e, evs)
	if !reflect.DeepEqual(before, after) {
		t.Fatal("重放结果受上一次上屏影响，确定性被破坏")
	}
	if got := e.DrainUserDelta(); len(got) == 0 {
		t.Fatal("上屏未产生用户词频增量")
	}
	if got := e.DrainUserDelta(); got != nil {
		t.Fatalf("增量应被一次性取走，重复取走得到 %v", got)
	}
}

func TestUserFrequencyPromotesCandidate(t *testing.T) {
	e := newTestEngine(t, Config{PageSize: 5})
	s := e.NewSession(SessionMeta{})
	before := texts(typeLetters(s, "shi"))
	if !reflect.DeepEqual(before, []string{"是", "时", "事"}) {
		t.Fatalf("初始排序 = %v", before)
	}
	idx := 2
	s.Handle(Event{Select: &idx})

	next := e.NewSession(SessionMeta{})
	after := texts(typeLetters(next, "shi"))
	if after[0] != "事" {
		t.Fatalf("上屏过的候选未上浮: %v", after)
	}
	delta := e.DrainUserDelta()
	if len(delta) != 1 || delta[0].Word != "事" || delta[0].Freq != 1 || delta[0].Pinyin == "" {
		t.Fatalf("增量记录不符合契约: %+v", delta)
	}
}

func TestUserFrequencyLoadsFromEntries(t *testing.T) {
	e, err := Open(testDict(t), []UserEntry{{Word: "事", Pinyin: "shi", Freq: 10, TS: 1}}, Config{PageSize: 5})
	if err != nil {
		t.Fatal(err)
	}
	s := e.NewSession(SessionMeta{})
	resp := typeLetters(s, "shi")
	if got := texts(resp); got[0] != "事" {
		t.Fatalf("载入的用户词频未生效: %v", got)
	}
}

func TestSelectOutOfPageRangeDoesNotCommit(t *testing.T) {
	e := newTestEngine(t, Config{PageSize: 3})
	s := e.NewSession(SessionMeta{})
	typeLetters(s, "shi")
	idx := 5
	resp := s.Handle(Event{Select: &idx})
	if resp.Commit != "" || resp.Preedit != "shi" {
		t.Fatalf("越界选择不应上屏: %+v", resp)
	}
}

func TestLettersAreCapped(t *testing.T) {
	e := newTestEngine(t, Config{PageSize: 5})
	s := e.NewSession(SessionMeta{})
	resp := typeLetters(s, "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz0123456789")
	if len(resp.Preedit) > maxLetters {
		t.Fatalf("预编辑长度 %d 超出上限 %d", len(resp.Preedit), maxLetters)
	}
	if resp.Caret != len(resp.Preedit) {
		t.Fatalf("caret %d 与预编辑长度 %d 不一致", resp.Caret, len(resp.Preedit))
	}
}

func TestCloseResetsSessionState(t *testing.T) {
	e := newTestEngine(t, Config{PageSize: 5})
	s := e.NewSession(SessionMeta{})
	typeLetters(s, "zhongguo")
	s.Handle(Event{Toggle: &chineseFull})
	s.Close()
	resp := press(s, KeySpace)
	if resp.Preedit != "" || resp.Commit != "" {
		t.Fatalf("Close 后会话仍带残留组合: %+v", resp)
	}
}

func TestKeyUpDoesNotEdit(t *testing.T) {
	e := newTestEngine(t, Config{PageSize: 5})
	s := e.NewSession(SessionMeta{})
	typeLetters(s, "ni")
	up := Key{Code: KeyA, Rune: 'a', Down: false}
	resp := s.Handle(Event{Key: &up})
	if resp.Preedit != "ni" {
		t.Fatalf("抬起事件改动了缓冲: %q", resp.Preedit)
	}
}
