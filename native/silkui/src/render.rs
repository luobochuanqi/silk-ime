//! 渲染后端：主路 GPU（flip 交换链 + D2D DeviceContext）与兜底路
//! （`ID2D1DCRenderTarget` + 预乘 alpha DIB + `UpdateLayeredWindow`）。
//!
//! 两者对外接口一致，场景绘制代码共享；区别只在合成路径与文字抗锯齿：
//! 主路是不透明目标，可用 ClearType；分层窗口是 per-pixel alpha，只能灰度。
//! 设备丢失（`D2DERR_RECREATE_TARGET` / DXGI device removed）由上层重建整个
//! [`Renderer`] 并重画当前帧。

use windows::core::{Interface, Result};
use windows::Win32::Foundation::{COLORREF, HMODULE, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1DCRenderTarget, ID2D1Device, ID2D1DeviceContext,
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1,
    D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_9_3,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_TEXT_ANTIALIAS_MODE, D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::IDWriteRenderingParams;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIOutput, IDXGISurface, IDXGISwapChain1, IDXGISwapChain2, DXGI_PRESENT,
    DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::Graphics::Gdi::{AC_SRC_ALPHA, AC_SRC_OVER};
use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};

use crate::scene::Scene;
use crate::theme::Theme;

/// 兜底路 D2D 工厂：整套渲染只在 UI 线程用，单线程工厂足够。
pub struct Factory(windows::Win32::Graphics::Direct2D::ID2D1Factory);

impl Factory {
    pub fn new() -> Result<Self> {
        // SAFETY: 无全局状态，工厂归调用线程所有。
        let f = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }?;
        Ok(Self(f))
    }
}

pub enum Renderer {
    Gpu(Gpu),
    Dcr(Dcr),
}

impl Renderer {
    /// 建主路；D3D 不可用（远程桌面老栈、驱动拒绝）时返回错误，由调用方退兜底路。
    pub fn new_gpu(hwnd: HWND, factory: &Factory, dpi: u32) -> Result<Self> {
        Ok(Self::Gpu(Gpu::new(hwnd, factory, dpi)?))
    }

    pub fn new_dcr(factory: &Factory, dpi: u32) -> Result<Self> {
        Ok(Self::Dcr(Dcr::new(factory, dpi)?))
    }

    /// 主路失败后是否应退到分层窗口：只有兜底路需要 `WS_EX_LAYERED`。
    pub fn needs_layered(&self) -> bool {
        matches!(self, Self::Dcr(_))
    }

    /// 尺寸/DPI 变化后重建目标（同一后端内原地调整）。
    pub fn resize(&mut self, hwnd: HWND, factory: &Factory, size: (i32, i32), dpi: u32) -> Result<()> {
        match self {
            Self::Gpu(g) => g.resize(hwnd, factory, size, dpi),
            Self::Dcr(d) => d.resize(size, dpi),
        }
    }

    /// 画一帧并呈现；`params` 是 DirectWrite 的 NATURAL 渲染参数。
    pub fn present(
        &mut self,
        hwnd: HWND,
        scene: &Scene,
        theme: &Theme,
        params: &IDWriteRenderingParams,
    ) -> Result<()> {
        match self {
            Self::Gpu(g) => g.present(scene, theme, params),
            Self::Dcr(d) => d.present(hwnd, scene, theme, params),
        }
    }
}

pub struct Gpu {
    device: ID3D11Device,
    context: ID2D1DeviceContext,
    swap: IDXGISwapChain1,
    bitmap: Option<ID2D1Bitmap1>,
    size: (u32, u32),
    dpi: u32,
}

impl Gpu {
    fn new(hwnd: HWND, factory: &Factory, dpi: u32) -> Result<Self> {
        let levels = [
            D3D_FEATURE_LEVEL_11_0,
            D3D_FEATURE_LEVEL_10_1,
            D3D_FEATURE_LEVEL_10_0,
            D3D_FEATURE_LEVEL_9_3,
        ];
        let mut device = None;
        let mut context = None;
        // SAFETY: 输出指针由 D3D 填写；驱动类型为硬件并允许系统软件回退。
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        let device = device.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
        // SAFETY: 设备刚创建，转换 QueryInterface 必然成功或明确失败。
        let dxgi_device: IDXGIDevice = device.cast()?;
        // SAFETY: DXGI 设备有效。
        let d2d_device: ID2D1Device = unsafe { windows::Win32::Graphics::Direct2D::D2D1CreateDevice(&dxgi_device, None)? };
        // SAFETY: 工厂与设备有效。
        let context = unsafe { d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)? };

        // 交换链必须由设备所属适配器的工厂创建（另开一个 DXGI 工厂会得到
        // DXGI_ERROR_INVALID_CALL）；这也是官方示例的做法。
        // SAFETY: 设备句柄有效，查询纯属读取。
        // SAFETY: 设备句柄有效，两次查询都是只读。
        let factory2: windows::Win32::Graphics::Dxgi::IDXGIFactory2 = unsafe {
            let adapter = dxgi_device.GetAdapter()?;
            adapter.GetParent()?
        };
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: 1,
            Height: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            // flip 模型 + 不透明：ClearType 成立的前提（06 主路）。
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: 0,
        };
        // SAFETY: 设备与窗口均有效；描述结构按 flip 模型填充。
        // SAFETY: 设备与窗口均有效；描述结构按 flip 模型填充。
        let swap =
            unsafe { factory2.CreateSwapChainForHwnd(&device, hwnd, &desc, None, None::<&IDXGIOutput>)? };
        // 帧延迟 1：候选窗按需重画，不需要排队帧。
        // 不限制帧率上限：候选窗按需重画，不等垂直同步会让输入更跟手。
        if let Ok(swap2) = swap.cast::<IDXGISwapChain2>() {
            // SAFETY: 接口有效；失败只影响延迟策略。
            unsafe {
                let _ = swap2.SetMaximumFrameLatency(1);
            }
        }
        let _ = factory;

        let mut gpu = Self {
            device,
            context,
            swap,
            bitmap: None,
            size: (0, 0),
            dpi,
        };
        gpu.recreate_target((1, 1))?;
        Ok(gpu)
    }

    fn recreate_target(&mut self, size: (u32, u32)) -> Result<()> {
        let (w, h) = (size.0.max(1), size.1.max(1));
        // SAFETY: 目标先解绑，缓冲才能 resize。
        unsafe {
            self.context.SetTarget(None);
        }
        self.bitmap = None;
        if self.size != (0, 0) && (w != self.size.0 || h != self.size.1) {
            // SAFETY: 所有缓冲引用已释放（bitmap 已置空）。
            // SAFETY: 所有缓冲引用已释放（bitmap 已置空）。
            unsafe {
                self.swap
                    .ResizeBuffers(0, w, h, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SWAP_CHAIN_FLAG(0))?;
            }
        }
        // SAFETY: 交换链有效。
        let surface: IDXGISurface = unsafe { self.swap.GetBuffer(0)? };
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: self.dpi as f32,
            dpiY: self.dpi as f32,
            // 交换链缓冲只作为输出：TARGET 之外必须标 CANNOT_DRAW，否则
            // CreateBitmapFromDxgiSurface 返回 E_INVALIDARG。
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            ..Default::default()
        };
        // SAFETY: surface 与上下文有效。
        // SAFETY: surface 与上下文有效。
        let bitmap = unsafe { self.context.CreateBitmapFromDxgiSurface(&surface, Some(&props))? };
        // SAFETY: 绑定目标并同步 DPI，之后所有坐标都是 DIP。
        unsafe {
            self.context.SetTarget(&bitmap);
            self.context.SetDpi(self.dpi as f32, self.dpi as f32);
        }
        self.bitmap = Some(bitmap);
        self.size = (w, h);
        Ok(())
    }

    fn resize(&mut self, _hwnd: HWND, _factory: &Factory, size: (i32, i32), dpi: u32) -> Result<()> {
        let size = (size.0.max(1) as u32, size.1.max(1) as u32);
        if self.size == size && self.dpi == dpi && self.bitmap.is_some() {
            return Ok(());
        }
        self.dpi = dpi;
        self.recreate_target(size)
    }

    fn present(
        &mut self,
        scene: &Scene,
        theme: &Theme,
        params: &IDWriteRenderingParams,
    ) -> Result<()> {
        let _ = &self.device;
        // SAFETY: 目标处于可绘制状态（上一帧 EndDraw 已完成）。
        unsafe {
            self.context.BeginDraw();
            scene.paint(&self.context, theme, GPU_TEXT_ANTIALIAS, params)?;
            self.context.EndDraw(None, None)?;
        }
        // SAFETY: 交换链有效。
        unsafe { self.swap.Present(0, DXGI_PRESENT(0)).ok() }
    }
}

pub struct Dcr {
    target: ID2D1DCRenderTarget,
    memdc: HDC,
    dib: Option<HBITMAP>,
    prev: HGDIOBJ,
    size: (i32, i32),
    dpi: u32,
}

impl Dcr {
    fn new(factory: &Factory, dpi: u32) -> Result<Self> {
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                // 分层窗口需要预乘 alpha（`UpdateLayeredWindow` 的 AC_SRC_ALPHA）。
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: dpi as f32,
            dpiY: dpi as f32,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        // SAFETY: 工厂有效，属性结构完整。
        let target = unsafe { factory.0.CreateDCRenderTarget(&props)? };
        // SAFETY: GDI 资源，失败返回空句柄（下方 DIB 创建会再失败一次）。
        let memdc = unsafe { CreateCompatibleDC(None) };
        if memdc.0.is_null() {
            return Err(windows::core::Error::from(windows::Win32::Foundation::E_OUTOFMEMORY));
        }
        Ok(Self {
            target,
            memdc,
            dib: None,
            prev: HGDIOBJ::default(),
            size: (0, 0),
            dpi,
        })
    }

    fn resize(&mut self, size: (i32, i32), dpi: u32) -> Result<()> {
        let w = size.0.max(1);
        let h = size.1.max(1);
        if self.size == (w, h) && self.dpi == dpi {
            return Ok(());
        }
        let header = BITMAPINFOHEADER {
            biSize: core::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            // 负高度 = 自上而下，与 D2D 的坐标方向一致。
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let info = BITMAPINFO {
            bmiHeader: header,
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
        // SAFETY: info 与输出指针有效；无共享内存段。
        let dib = unsafe {
            CreateDIBSection(Some(self.memdc), &info, DIB_RGB_COLORS, &mut bits, None, 0)?
        };
        if bits.is_null() {
            // SAFETY: 句柄已创建但不可用，立即释放避免泄漏。
            unsafe { let _ = DeleteObject(HGDIOBJ(dib.0)); }
            return Err(windows::core::Error::from(windows::Win32::Foundation::E_OUTOFMEMORY));
        }
        // SAFETY: 替换 DC 里的位图，旧位图随后释放。
        let prev = unsafe { SelectObject(self.memdc, HGDIOBJ(dib.0)) };
        if let Some(old) = self.dib.replace(dib) {
            // SAFETY: 旧 DIB 已不在 DC 中；prev 保存的是最初的 1x1 占位。
            unsafe { let _ = DeleteObject(HGDIOBJ(old.0)); }
        }
        self.prev = prev;
        // SAFETY: 目标与 DC 有效，绑定整块位图。
        unsafe {
            self.target.BindDC(
                self.memdc,
                &RECT {
                    left: 0,
                    top: 0,
                    right: w,
                    bottom: h,
                },
            )?;
            self.target.SetDpi(dpi as f32, dpi as f32);
        }
        self.size = (w, h);
        self.dpi = dpi;
        Ok(())
    }

    fn present(
        &mut self,
        hwnd: HWND,
        scene: &Scene,
        theme: &Theme,
        params: &IDWriteRenderingParams,
    ) -> Result<()> {
        if self.size.0 <= 1 && self.size.1 <= 1 {
            return Ok(());
        }
        // SAFETY: 目标已绑定当前尺寸的 DIB。
        unsafe {
            self.target.BeginDraw();
            scene.paint(&self.target, theme, DCR_TEXT_ANTIALIAS, params)?;
            self.target.EndDraw(None, None)?;
        }
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let size = SIZE {
            cx: self.size.0,
            cy: self.size.1,
        };
        let origin = POINT { x: 0, y: 0 };
        // SAFETY: memdc 持有与 size 匹配的预乘 alpha DIB。
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                None,
                None,
                Some(&size),
                Some(self.memdc),
                Some(&origin),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )?;
        }
        Ok(())
    }
}

impl Drop for Dcr {
    fn drop(&mut self) {
        // SAFETY: 释放 GDI 资源；DC 与位图不再被使用。
        unsafe {
            if let Some(dib) = self.dib.take() {
                let _ = SelectObject(self.memdc, self.prev);
                let _ = DeleteObject(HGDIOBJ(dib.0));
            }
            let _ = DeleteDC(self.memdc);
        }
    }
}

/// 主路文字抗锯齿模式（不透明目标 → ClearType）。
pub const GPU_TEXT_ANTIALIAS: D2D1_TEXT_ANTIALIAS_MODE = D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE;
/// 兜底路文字抗锯齿模式（alpha DIB → 灰度）。
pub const DCR_TEXT_ANTIALIAS: D2D1_TEXT_ANTIALIAS_MODE = D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE;

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WS_EX_TOOLWINDOW, WS_POPUP,
    };

    unsafe extern "system" fn proc_(
        h: HWND,
        m: u32,
        w: windows::Win32::Foundation::WPARAM,
        l: windows::Win32::Foundation::LPARAM,
    ) -> windows::Win32::Foundation::LRESULT {
        windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(h, m, w, l)
    }

    /// 建一个已注册类、未显示的 popup：交换链要求真实（非空）窗口句柄。
    fn test_window() -> HWND {
        let class: Vec<u16> = "SilkUiRenderProbe\0".encode_utf16().collect();
        // SAFETY: 测试进程内建一个不可见 popup 用于验证后端创建。
        unsafe {
            let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap();
            let wc = windows::Win32::UI::WindowsAndMessaging::WNDCLASSW {
                lpfnWndProc: Some(proc_),
                hInstance: hinst.into(),
                lpszClassName: windows::core::PCWSTR(class.as_ptr()),
                ..Default::default()
            };
            windows::Win32::UI::WindowsAndMessaging::RegisterClassW(&wc);
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                windows::core::PCWSTR(class.as_ptr()),
                windows::core::PCWSTR(class.as_ptr()),
                WS_POPUP,
                0,
                0,
                10,
                10,
                None,
                None,
                Some(hinst.into()),
                None,
            )
            .unwrap_or_default()
        }
    }

    #[test]
    fn at_least_one_backend_constructs() {
        let factory = Factory::new().unwrap();
        let hwnd = test_window();
        let gpu = Renderer::new_gpu(hwnd, &factory, 96);
        let dcr = Renderer::new_dcr(&factory, 96);
        assert!(
            gpu.is_ok() || dcr.is_ok(),
            "两条渲染后端都建不起来：gpu={:?} dcr={:?}",
            gpu.err(),
            dcr.err()
        );
        if !hwnd.is_invalid() {
            // SAFETY: 测试窗口由本线程创建。
            unsafe { let _ = DestroyWindow(hwnd); }
        }
    }
}
