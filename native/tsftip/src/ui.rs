//! 候选 UI 元素与显示属性（TSF 的 UIless 接入点）。
//!
//! 候选窗本身由 silkui 绘制；这里实现的是系统需要的登记对象：候选列表元素
//! 让 light dismiss、全屏/游戏抑制、触摸键盘策略生效（03 的"必须"项），显示属性
//! 提供 preedit 下划线。两者都只持有快照，不发起 IPC。

use std::sync::{Arc, Mutex, Weak};

use windows::core::{implement, Interface, BSTR, GUID, Result};
use windows::Win32::UI::TextServices::{
    ITfCandidateListUIElement, ITfCandidateListUIElement_Impl, ITfCategoryMgr, ITfContext,
    ITfDisplayAttributeInfo, ITfDisplayAttributeInfo_Impl, ITfDisplayAttributeProvider,
    ITfDisplayAttributeProvider_Impl, ITfDocumentMgr, ITfUIElement, ITfUIElement_Impl,
    ITfUIElementMgr, TF_CLUIE_COUNT, TF_CLUIE_CURRENTPAGE, TF_CLUIE_DOCUMENTMGR,
    TF_CLUIE_SELECTION, TF_CLUIE_STRING, TF_CT_NONE,
    TF_LS_SOLID, TF_ATTR_INPUT, TF_DA_COLOR, TF_DISPLAYATTRIBUTE,
};

use crate::msg::Candidate;
use crate::session::Session;

/// preedit 的显示属性 GUID。注册到类别管理器后，GUID_PROP_ATTRIBUTE 的值引用它。
pub const GUID_SILK_DISPLAY_ATTRIBUTE: GUID =
    GUID::from_u128(0x0e5f6a7c_8c9d_4e0f_a1b2_c3d4e5f6a7b8);

/// 候选元素的内容快照。
#[derive(Debug, Clone, Default)]
pub struct View {
    pub candidates: Vec<Candidate>,
    pub page_index: u32,
    pub page_size: u32,
    pub selected: u32,
}

/// 元素的候选快照。与 session 共享，内容变化时宿主已持有的元素仍能看到新数据。
pub type SharedView = Arc<Mutex<View>>;

#[implement(ITfCandidateListUIElement)]
pub struct CandidateElement {
    pub view: SharedView,
    pub session: Weak<Session>,
    pub doc_mgr: Option<ITfDocumentMgr>,
}

impl CandidateElement {
    fn view(&self) -> View {
        self.view.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl ITfUIElement_Impl for CandidateElement_Impl {
    fn GetDescription(&self) -> Result<BSTR> {
        Ok(BSTR::from("Silk 候选"))
    }

    fn GetGUID(&self) -> Result<GUID> {
        Ok(GUID::zeroed())
    }

    fn Show(&self, show: windows::core::BOOL) -> Result<()> {
        if let Some(session) = self.session.upgrade() {
            session.lock().shown = show.as_bool();
        }
        Ok(())
    }

    fn IsShown(&self) -> Result<windows::core::BOOL> {
        let shown = self
            .session
            .upgrade()
            .map(|session| session.lock().shown)
            .unwrap_or(false);
        Ok(windows::core::BOOL::from(shown))
    }
}

impl ITfCandidateListUIElement_Impl for CandidateElement_Impl {
    fn GetUpdatedFlags(&self) -> Result<u32> {
        Ok(TF_CLUIE_STRING
            | TF_CLUIE_COUNT
            | TF_CLUIE_SELECTION
            | TF_CLUIE_CURRENTPAGE
            | TF_CLUIE_DOCUMENTMGR)
    }

    fn GetDocumentMgr(&self) -> Result<ITfDocumentMgr> {
        self.doc_mgr
            .clone()
            .ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_POINTER))
    }

    fn GetCount(&self) -> Result<u32> {
        Ok(self.view().candidates.len() as u32)
    }

    fn GetSelection(&self) -> Result<u32> {
        Ok(self.view().selected)
    }

    fn GetString(&self, index: u32) -> Result<BSTR> {
        self.view()
            .candidates
            .get(index as usize)
            .map(|candidate| BSTR::from(candidate.text.as_str()))
            .ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_INVALIDARG))
    }

    fn GetPageIndex(&self, index: *mut u32, capacity: u32, page_count: *mut u32) -> Result<()> {
        if index.is_null() || page_count.is_null() {
            return Err(windows::core::Error::from(windows::Win32::Foundation::E_POINTER));
        }
        // 平铺分页：页首页索引 = page * page_size。
        let view = self.view();
        let size = view.page_size.max(1);
        let pages = (view.candidates.len() as u32).div_ceil(size);
        // SAFETY: TSF 传进来的两个指针按接口契约有效：page_count 可写 1 个 u32，
        // index 数组有 capacity 个 u32；写入数量取 capacity.min(pages)，不越界。
        unsafe {
            *page_count = pages;
            for page in 0..capacity.min(pages) {
                index.add(page as usize).write(page * size);
            }
        }
        Ok(())
    }

    fn SetPageIndex(&self, _index: *const u32, _count: u32) -> Result<()> {
        // 翻页由输入法自己处理（PageUp/PageDown），系统改页不生效。
        Ok(())
    }

    fn GetCurrentPage(&self) -> Result<u32> {
        Ok(self.view().page_index)
    }
}

/// 按当前状态登记或更新候选元素。
pub fn sync(session: &Arc<Session>, ctx: &ITfContext) {
    let Ok(mgr) = ctx.cast::<ITfUIElementMgr>() else {
        return;
    };
    let (candidates, page_index, selected, existing) = {
        let state = session.lock();
        (
            state.candidates.clone(),
            state.page_index,
            state.selected,
            state.ui_id.zip(state.ui_view.clone()),
        )
    };
    if candidates.is_empty() {
        end(session, ctx);
        return;
    }
    let view = View {
        candidates,
        page_index,
        page_size: session.page_size(),
        selected,
    };
    match existing {
        Some((id, shared)) => {
            // 宿主已持有元素指针：换掉共享快照再通知刷新，避免 End+Begins 闪烁。
            *shared.lock().unwrap_or_else(|e| e.into_inner()) = view;
            // SAFETY: mgr 由当前 context 取得；id 来自成功的 BeginUIElement。
            let _ = unsafe { mgr.UpdateUIElement(id) };
        }
        None => {
            let (doc_mgr, shown) = {
                let state = session.lock();
                (state.doc_mgr.clone(), state.shown)
            };
            let shared: SharedView = Arc::new(Mutex::new(view));
            let element: CandidateElement = CandidateElement {
                view: Arc::clone(&shared),
                session: Arc::downgrade(session),
                doc_mgr,
            };
            let list: ITfCandidateListUIElement = element.into();
            let ui: ITfUIElement = list.into();
            let mut show = windows::core::BOOL::from(shown);
            let mut id = 0u32;
            // SAFETY: mgr 有效；ui 是本站对象的接口引用，TSF 会持有它到 EndUIElement；
            // show/id 是本地可写量。
            if unsafe { mgr.BeginUIElement(&ui, &mut show, &mut id) }.is_ok() {
                let mut state = session.lock();
                state.ui_id = Some(id);
                state.ui_view = Some(shared);
                state.shown = show.as_bool();
            }
        }
    }
}

/// 结束候选元素登记（失焦/提交）。
pub fn end(session: &Arc<Session>, ctx: &ITfContext) {
    let Ok(mgr) = ctx.cast::<ITfUIElementMgr>() else {
        return;
    };
    let id = {
        let mut state = session.lock();
        state.ui_view = None;
        state.ui_id.take()
    };
    if let Some(id) = id {
        // SAFETY: mgr 有效；id 是本会话登记且尚未结束的那一个。
        let _: windows::core::Result<()> = unsafe { mgr.EndUIElement(id) };
    }
    session.lock().shown = false;
}

/// preedit 下划线属性（GUID_PROP_ATTRIBUTE 引用的显示属性）。
#[implement(ITfDisplayAttributeInfo)]
pub struct DisplayAttribute;

impl ITfDisplayAttributeInfo_Impl for DisplayAttribute_Impl {
    fn GetGUID(&self) -> Result<GUID> {
        Ok(GUID_SILK_DISPLAY_ATTRIBUTE)
    }

    fn GetDescription(&self) -> Result<BSTR> {
        Ok(BSTR::from("Silk 输入中"))
    }

    fn GetAttributeInfo(&self, info: *mut TF_DISPLAYATTRIBUTE) -> Result<()> {
        if info.is_null() {
            return Err(windows::core::Error::from(windows::Win32::Foundation::E_POINTER));
        }
        let none = TF_DA_COLOR {
            r#type: TF_CT_NONE,
            ..Default::default()
        };
        let value = TF_DISPLAYATTRIBUTE {
            crText: none,
            crBk: none,
            lsStyle: TF_LS_SOLID,
            fBoldLine: windows::core::BOOL::from(false),
            crLine: none,
            bAttr: TF_ATTR_INPUT,
        };
        // SAFETY: info 非空（上面判过），按接口契约指向调用方可写的 1 个结构。
        unsafe { info.write(value) };
        Ok(())
    }

    fn SetAttributeInfo(&self, _info: *const TF_DISPLAYATTRIBUTE) -> Result<()> {
        Ok(())
    }

    fn Reset(&self) -> Result<()> {
        Ok(())
    }
}

/// 显示属性提供者，挂在 COM 类厂上供 TSF 查询。
#[implement(ITfDisplayAttributeProvider)]
pub struct DisplayAttributeProvider;

impl ITfDisplayAttributeProvider_Impl for DisplayAttributeProvider_Impl {
    fn EnumDisplayAttributeInfo(
        &self,
    ) -> Result<windows::Win32::UI::TextServices::IEnumTfDisplayAttributeInfo> {
        Err(windows::core::Error::from(windows::Win32::Foundation::E_NOTIMPL))
    }

    fn GetDisplayAttributeInfo(&self, guid: *const GUID) -> Result<ITfDisplayAttributeInfo> {
        // SAFETY: guid 由 TSF 按接口契约传入；非空才解引用。
        if guid.is_null() || unsafe { *guid } != GUID_SILK_DISPLAY_ATTRIBUTE {
            return Err(windows::core::Error::from(windows::Win32::Foundation::E_INVALIDARG));
        }
        let info: DisplayAttribute = DisplayAttribute;
        Ok(info.into())
    }
}

/// 把显示属性登记进类别管理器；值是指向本类的 GUID 原子，供属性值引用。
pub fn register_display_attribute() -> bool {
    // SAFETY: CoCreateInstance 只返回新接口指针；参数为静态 CLSID 与进程内服务器。
    let mgr: Result<ITfCategoryMgr> = unsafe {
        windows::Win32::System::Com::CoCreateInstance(
            &windows::Win32::UI::TextServices::CLSID_TF_CategoryMgr,
            None,
            windows::Win32::System::Com::CLSCTX_INPROC_SERVER,
        )
    };
    let Ok(mgr) = mgr else {
        return false;
    };
    // SAFETY: mgr 有效；GUID/CLSID 均为静态常量，两个调用只读写 TSF 自己的注册表。
    unsafe {
        mgr.RegisterGUID(&GUID_SILK_DISPLAY_ATTRIBUTE).is_ok()
            && mgr
                .RegisterCategory(
                    &crate::reg::CLSID_SILK_TIP,
                    &windows::Win32::UI::TextServices::GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
                    &GUID_SILK_DISPLAY_ATTRIBUTE,
                )
                .is_ok()
    }
}
