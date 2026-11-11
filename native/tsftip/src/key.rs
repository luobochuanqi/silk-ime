//! 虚拟键到协议键码的翻译与拦截决策。
//!
//! 键码与 Go 引擎一一对应（数值不可改）；拦截决策只看本地缓存模式与是否组合中，
//! 不碰协议与 COM，因此可以在无 TSF 环境里单测。

use windows::Win32::UI::Input::KeyboardAndMouse::*;

/// 协议键码，数值与 internal/engine 的 KeyCode 一致。
pub const KEY_A: u16 = 1;
pub const KEY_0: u16 = 27;
pub const KEY_SPACE: u16 = 37;
pub const KEY_ENTER: u16 = 38;
pub const KEY_BACKSPACE: u16 = 39;
pub const KEY_ESC: u16 = 40;
pub const KEY_PAGE_UP: u16 = 41;
pub const KEY_PAGE_DOWN: u16 = 42;
pub const KEY_PUNCT: u16 = 43;

/// 修饰键位掩码，与引擎 ModMask 一致。
pub const MOD_SHIFT: u8 = 1;
pub const MOD_CTRL: u8 = 2;
pub const MOD_ALT: u8 = 4;

/// 输入模式快照；以 host 为权威，这里只是最终副本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub chinese: bool,
    pub full_punct: bool,
}

impl Default for Mode {
    fn default() -> Self {
        // welcome 之前按中文全角处理：用户装了输入法就是要打中文。
        Mode {
            chinese: true,
            full_punct: true,
        }
    }
}

/// 一次按键的处置方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// 放行给应用，零 IPC。
    Pass,
    /// 中英切换，需要向 host 发一次 action。
    Toggle,
    /// 进引擎路径。
    Forward(u16),
}

/// 从按键状态推导修饰位。
pub fn mods_from_state(shift: bool, ctrl: bool, alt: bool) -> u8 {
    (if shift { MOD_SHIFT } else { 0 })
        | (if ctrl { MOD_CTRL } else { 0 })
        | (if alt { MOD_ALT } else { 0 })
}

/// 读当前按键状态；TSF 回调线程是宿主 UI 线程，可以直接取同步状态。
pub fn current_mods() -> u8 {
    // SAFETY: GetKeyState 只按 VK 读线程的同步按键状态，无指针参数、无失败路径；
    // TSF 回调就在宿主 UI 线程上，状态与正在处理的按键一致。
    let pressed = |vk: VIRTUAL_KEY| unsafe { GetKeyState(vk.0 as i32) < 0 };
    mods_from_state(
        pressed(VK_SHIFT),
        pressed(VK_CONTROL),
        pressed(VK_MENU),
    )
}

/// VK 到字母/数字键码。非字母数字返回 None。
pub fn alpha_digit_code(vk: u16) -> Option<u16> {
    match vk {
        v if (VK_A.0..=VK_Z.0).contains(&v) => Some(KEY_A + (v - VK_A.0)),
        v if (VK_0.0..=VK_9.0).contains(&v) => Some(KEY_0 + (v - VK_0.0)),
        _ => None,
    }
}

/// 标点键判定：覆盖 OEM 符号键与逗号/句号/等号/减号。
pub fn is_punct(vk: u16) -> bool {
    matches!(
        vk,
        v if v == VK_OEM_1.0
            || v == VK_OEM_2.0
            || v == VK_OEM_3.0
            || v == VK_OEM_4.0
            || v == VK_OEM_5.0
            || v == VK_OEM_6.0
            || v == VK_OEM_7.0
            || v == VK_OEM_COMMA.0
            || v == VK_OEM_PERIOD.0
            || v == VK_OEM_PLUS.0
            || v == VK_OEM_MINUS.0
    )
}

/// 编辑键到协议键码。
fn edit_code(vk: u16) -> Option<u16> {
    match vk {
        v if v == VK_SPACE.0 => Some(KEY_SPACE),
        v if v == VK_RETURN.0 => Some(KEY_ENTER),
        v if v == VK_BACK.0 => Some(KEY_BACKSPACE),
        v if v == VK_ESCAPE.0 => Some(KEY_ESC),
        v if v == VK_PRIOR.0 => Some(KEY_PAGE_UP),
        v if v == VK_NEXT.0 => Some(KEY_PAGE_DOWN),
        _ => None,
    }
}

/// 拦截决策。
///
/// 英文模式一律放行（零 IPC，英文补全不在本层）；编辑键与数字键只在组合期拦截，
/// 否则交给应用，避免无谓的 15 ms 往返。Ctrl/Alt 组合留给系统与宿主。
pub fn decide(vk: u16, mods: u8, mode: Mode, composing: bool) -> Decision {
    if mods & (MOD_CTRL | MOD_ALT) != 0 {
        return Decision::Pass;
    }
    if !mode.chinese {
        return Decision::Pass;
    }
    if let Some(code) = alpha_digit_code(vk) {
        let digit = code >= KEY_0;
        if digit && !composing {
            return Decision::Pass;
        }
        return Decision::Forward(code);
    }
    if let Some(code) = edit_code(vk) {
        return if composing {
            Decision::Forward(code)
        } else {
            Decision::Pass
        };
    }
    if is_punct(vk) && (composing || mode.full_punct) {
        return Decision::Forward(KEY_PUNCT);
    }
    Decision::Pass
}

/// 响应回来后是否吞掉这次按键（OnKeyDown 的 *pfEaten）。
///
/// 协议 `update` 不带引擎的 Consume 位，因此本地按「有 preedit / 有上屏」判定；
/// 组合期按 Esc、退格、翻页等编辑键即使清空了内容也必须吞掉，否则应用会收到
/// 本应被输入法处理的按键。
pub fn should_consume(preedit: &str, commit: &str, had_composition: bool, code: u16) -> bool {
    if !preedit.is_empty() || !commit.is_empty() {
        return true;
    }
    had_composition
        && matches!(code, KEY_ESC | KEY_BACKSPACE | KEY_PAGE_UP | KEY_PAGE_DOWN)
}

/// 每个虚拟键「按下时是否被吞」的记账，供抬起时复用同一决定。
///
/// 04:112 要求拿不准就放行：TestKeyUp 若按当前模式重新判定，会在「按下被吞、
/// 期间模式或组合状态变了」时放走孤儿 KeyUp，反之则吞掉本不该吞的抬起。记住
/// 按下时的实际结果，抬起照抄即可。
#[derive(Debug)]
pub struct EatenKeys {
    slots: [bool; 256],
}

// 超过 32 元素的数组没有 Default 派生（MSRV 1.74），手写一份全 false。
impl Default for EatenKeys {
    fn default() -> EatenKeys {
        EatenKeys { slots: [false; 256] }
    }
}

impl EatenKeys {
    pub fn remember(&mut self, vk: u16, eaten: bool) {
        if let Some(slot) = self.slots.get_mut(vk as usize) {
            *slot = eaten;
        }
    }

    /// 按下时的处置结果（抬起要在 Test 与 Do 两个回调里给出同一答案）。
    pub fn peek(&self, vk: u16) -> bool {
        self.slots.get(vk as usize).copied().unwrap_or(false)
    }

}

/// Shift 单键轻按（按下后未插入其他键即抬起）用于中英切换。
///
/// 放在 shim 而不是 host：切换必须在按键瞬间决定是否吞键，走一次 15 ms 往返
/// 会让首键体验抖动，且模式本身以 host 回传为准。
#[derive(Debug, Default)]
pub struct ShiftTap {
    down: bool,
    clean: bool,
}

impl ShiftTap {
    /// KeyDown 时调用；返回 true 表示这次按下被跟踪。
    pub fn on_down(&mut self, vk: u16) -> bool {
        if vk != VK_SHIFT.0 {
            self.clean = false;
            return false;
        }
        self.down = true;
        self.clean = true;
        false
    }

    /// KeyUp 时调用；返回 true 表示应触发中英切换。
    pub fn on_up(&mut self, vk: u16) -> bool {
        if vk != VK_SHIFT.0 {
            self.clean = false;
            return false;
        }
        let tap = self.down && self.clean;
        self.down = false;
        self.clean = false;
        tap
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CN: Mode = Mode {
        chinese: true,
        full_punct: true,
    };
    const EN: Mode = Mode {
        chinese: false,
        full_punct: true,
    };

    #[test]
    fn keycodes_mirror_engine() {
        assert_eq!(alpha_digit_code(VK_A.0), Some(1));
        assert_eq!(alpha_digit_code(VK_Z.0), Some(26));
        assert_eq!(alpha_digit_code(VK_0.0), Some(27));
        assert_eq!(alpha_digit_code(VK_9.0), Some(36));
        assert_eq!(KEY_SPACE, 37);
        assert_eq!(KEY_ENTER, 38);
        assert_eq!(KEY_BACKSPACE, 39);
        assert_eq!(KEY_ESC, 40);
        assert_eq!(KEY_PAGE_UP, 41);
        assert_eq!(KEY_PAGE_DOWN, 42);
        assert_eq!(KEY_PUNCT, 43);
        assert_eq!(alpha_digit_code(VK_F1.0), None);
    }

    #[test]
    fn mods_bits() {
        assert_eq!(mods_from_state(true, false, false), MOD_SHIFT);
        assert_eq!(mods_from_state(true, true, true), 7);
        assert_eq!(mods_from_state(false, false, false), 0);
    }

    #[test]
    fn english_mode_never_intercepts() {
        for vk in [VK_A.0, VK_0.0, VK_SPACE.0, VK_OEM_1.0, VK_ESCAPE.0] {
            assert_eq!(decide(vk, 0, EN, true), Decision::Pass);
            assert_eq!(decide(vk, 0, EN, false), Decision::Pass);
        }
    }

    #[test]
    fn ctrl_alt_combos_pass() {
        assert_eq!(decide(VK_A.0, MOD_CTRL, CN, true), Decision::Pass);
        assert_eq!(decide(VK_SPACE.0, MOD_ALT, CN, true), Decision::Pass);
    }

    #[test]
    fn letters_forward_always_in_chinese() {
        assert_eq!(decide(VK_H.0, 0, CN, false), Decision::Forward(8));
        assert_eq!(decide(VK_H.0, MOD_SHIFT, CN, false), Decision::Forward(8));
    }

    #[test]
    fn digits_and_edits_only_while_composing() {
        assert_eq!(decide(VK_1.0, 0, CN, false), Decision::Pass);
        assert_eq!(decide(VK_1.0, 0, CN, true), Decision::Forward(28));
        assert_eq!(decide(VK_SPACE.0, 0, CN, false), Decision::Pass);
        assert_eq!(decide(VK_SPACE.0, 0, CN, true), Decision::Forward(KEY_SPACE));
        assert_eq!(decide(VK_BACK.0, 0, CN, true), Decision::Forward(KEY_BACKSPACE));
    }

    #[test]
    fn punctuation_follows_full_punct_mode() {
        assert_eq!(decide(VK_OEM_COMMA.0, 0, CN, false), Decision::Forward(KEY_PUNCT));
        let half = Mode {
            chinese: true,
            full_punct: false,
        };
        assert_eq!(decide(VK_OEM_COMMA.0, 0, half, false), Decision::Pass);
        assert_eq!(decide(VK_OEM_COMMA.0, 0, half, true), Decision::Forward(KEY_PUNCT));
    }

    #[test]
    fn consume_rule() {
        assert!(should_consume("hao", "", false, KEY_A));
        assert!(should_consume("", "好", false, KEY_SPACE));
        assert!(!should_consume("", "", false, KEY_SPACE));
        assert!(should_consume("", "", true, KEY_ESC));
        assert!(should_consume("", "", true, KEY_BACKSPACE));
        assert!(!should_consume("", "", true, KEY_A));
        assert!(!should_consume("", "", false, KEY_ESC));
    }

    #[test]
    fn eaten_keys_mirror_the_press() {
        let mut keys = EatenKeys::default();
        // 没按过的键抬起一律放行（拿不准就放行）。
        assert!(!keys.peek(VK_A.0));
        // 按下被吞 → 抬起被吞。
        keys.remember(VK_A.0, true);
        assert!(keys.peek(VK_A.0));
        // 抬起处理完清账，下一次同键不再被吞。
        keys.remember(VK_A.0, false);
        assert!(!keys.peek(VK_A.0));
        // 按下放行的键，抬起也放行。
        keys.remember(VK_SPACE.0, false);
        assert!(!keys.peek(VK_SPACE.0));
    }

    #[test]
    fn shift_tap_requires_clean_press() {
        let mut tap = ShiftTap::default();
        assert!(!tap.on_down(VK_SHIFT.0));
        assert!(tap.on_up(VK_SHIFT.0));

        let mut tap = ShiftTap::default();
        tap.on_down(VK_SHIFT.0);
        tap.on_down(VK_A.0);
        assert!(!tap.on_up(VK_SHIFT.0));

        let mut tap = ShiftTap::default();
        assert!(!tap.on_up(VK_SHIFT.0));
    }
}
