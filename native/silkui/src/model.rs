//! 视图快照：把 C ABI 的 `SilkView` 拷成独立所有权数据。
//!
//! 指针只在导出函数调用期间有效（契约），因此必须在进入 UI 线程前完成拷贝；
//! 同时按上限截断，避免畸形输入把排版预算打穿（兜底路跑在宿主进程内）。

use core::ffi::c_char;
use std::ffi::CStr;

use crate::SilkView;

/// 单页候选上限：超过部分不显示（键盘路径本就只覆盖 1..9，翻页由 shim 负责）。
const MAX_CANDIDATES: usize = 32;
/// 单个字符串上限（UTF-8 字节）：超长输入无显示价值，且排版成本随长度线性增长。
const MAX_TEXT_BYTES: usize = 512;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub text: String,
    pub comment: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub preedit: String,
    pub rows: Vec<Row>,
    pub page_index: i32,
}

impl Snapshot {
    /// 从宿主视图拷贝。`view` 为空指针返回 `None`（调用方据此隐藏窗口）。
    ///
    /// # Safety
    /// `view` 及其内部指针要么为空，要么指向调用期间有效的 NUL 结尾 UTF-8 字符串
    /// 与长度匹配的候选数组——这是 C ABI 契约，宿主负责保证。
    pub unsafe fn copy_from(view: *const SilkView) -> Option<Self> {
        if view.is_null() {
            return None;
        }
        // SAFETY: 契约保证 `view` 在调用期间有效。
        let view = unsafe { &*view };
        let preedit = read_string(view.preedit);
        let mut rows = Vec::new();
        if !view.candidates.is_null() && view.count > 0 {
            let count = (view.count as usize).min(MAX_CANDIDATES);
            // SAFETY: 契约保证数组至少 `count` 个元素。
            let candidates = unsafe { core::slice::from_raw_parts(view.candidates, count) };
            for c in candidates {
                rows.push(Row {
                    text: read_string(c.text),
                    comment: read_string(c.comment),
                });
            }
        }
        Some(Self {
            preedit,
            rows,
            page_index: view.page_index,
        })
    }

    /// 无可显示内容：空候选且无组字串 → 窗口应隐藏（06 可见性状态机）。
    pub fn is_empty(&self) -> bool {
        self.rows.iter().all(|r| r.text.is_empty()) && self.preedit.is_empty()
    }
}

/// 读取 NUL 结尾 UTF-8；空指针、非法 UTF-8、超长都按安全值处理。
///
/// # Safety
/// 非空指针必须指向 NUL 结尾的有效字符串。
unsafe fn read_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: 由调用方契约保证。
    let bytes = unsafe { CStr::from_ptr(ptr) }.to_bytes();
    let bytes = if bytes.len() > MAX_TEXT_BYTES {
        &bytes[..floor_char_boundary(bytes, MAX_TEXT_BYTES)]
    } else {
        bytes
    };
    String::from_utf8_lossy(bytes).into_owned()
}

/// 找到不超过 `max` 的字符边界，避免在多字节序列中间切断。
fn floor_char_boundary(bytes: &[u8], max: usize) -> usize {
    let mut end = max.min(bytes.len());
    while end > 0 && end < bytes.len() && (bytes[end] & 0xC0) == 0x80 {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn floor_char_boundary_never_splits_utf8() {
        let s = "候选词".as_bytes();
        assert_eq!(floor_char_boundary(s, 4), 3);
        assert_eq!(floor_char_boundary(s, 3), 3);
        assert_eq!(floor_char_boundary(s, 100), s.len());
        assert_eq!(floor_char_boundary(s, 0), 0);
    }

    #[test]
    fn read_string_handles_null_and_truncation() {
        // SAFETY: 空指针按契约允许。
        assert_eq!(unsafe { read_string(core::ptr::null()) }, "");
        let long = CString::new("候".repeat(400)).unwrap();
        // SAFETY: CString 保证 NUL 结尾且存活。
        let s = unsafe { read_string(long.as_ptr()) };
        assert!(s.len() <= MAX_TEXT_BYTES);
        assert!(s.chars().all(|c| c == '候'));
    }

    #[test]
    fn copy_from_reads_candidates_and_rejects_empty() {
        let pre = CString::new("hao").unwrap();
        let texts = [CString::new("好").unwrap(), CString::new("号").unwrap()];
        let comments = [CString::new("").unwrap(), CString::new("hào").unwrap()];
        let cands: Vec<crate::SilkCandidate> = texts
            .iter()
            .zip(&comments)
            .map(|(t, c)| crate::SilkCandidate {
                text: t.as_ptr(),
                comment: c.as_ptr(),
            })
            .collect();
        let view = SilkView {
            preedit: pre.as_ptr(),
            candidates: cands.as_ptr(),
            count: cands.len() as i32,
            page_index: 2,
        };
        // SAFETY: 所有指针指向存活到调用结束的 CString。
        let snap = unsafe { Snapshot::copy_from(&view) }.unwrap();
        assert_eq!(snap.preedit, "hao");
        assert_eq!(snap.rows.len(), 2);
        assert_eq!(snap.rows[1].comment, "hào");
        assert_eq!(snap.page_index, 2);
        assert!(!snap.is_empty());

        let blank = CString::new("").unwrap();
        let empty = SilkView {
            preedit: blank.as_ptr(),
            candidates: core::ptr::null(),
            count: 0,
            page_index: 0,
        };
        // SAFETY: 同上，空候选是合法输入。
        assert!(unsafe { Snapshot::copy_from(&empty) }.unwrap().is_empty());
        // SAFETY: 空视图指针按契约返回 None。
        assert!(unsafe { Snapshot::copy_from(core::ptr::null()) }.is_none());
    }

    #[test]
    fn negative_count_is_ignored() {
        let view = SilkView {
            preedit: core::ptr::null(),
            candidates: core::ptr::null(),
            count: -3,
            page_index: 0,
        };
        // SAFETY: 空指针 + 负计数是畸形但必须不 panic 的输入。
        let snap = unsafe { Snapshot::copy_from(&view) }.unwrap();
        assert!(snap.rows.is_empty());
    }
}
