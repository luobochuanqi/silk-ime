//! DirectWrite 排版：字体、度量与省略号截断。
//!
//! 主路与兜底路共用同一份排版（06 的硬约束），因此这里只产出
//! `IDWriteTextLayout`，不关心最终往哪块目标绘制。所有坐标与字号都是
//! **DIP**：DPI 由渲染目标的 `SetDpi` 承担换算，排版层与 DPI 无关。

use windows::core::{Result, PCWSTR};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteInlineObject, IDWriteRenderingParams,
    IDWriteTextFormat, IDWriteTextLayout, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_PIXEL_GEOMETRY_FLAT, DWRITE_RENDERING_MODE_NATURAL, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP,
};

use crate::layout::{TextStyle, METRICS};

/// 字体族：微软雅黑；缺字由系统字形回退补齐（06 字体规则）。
const FONT_FAMILY: &str = "Microsoft YaHei";
/// 建布局时的宽度上界（DIP）：先给足，随后收缩到真实列宽做省略号截断。
const MEASURE_WIDTH: f32 = 4096.0;

pub struct TextEngine {
    factory: IDWriteFactory,
    formats: [IDWriteTextFormat; 2],
    ellipsis: [IDWriteInlineObject; 2],
    params: IDWriteRenderingParams,
}

impl TextEngine {
    pub fn new() -> Result<Self> {
        // SAFETY: 该 UI 线程已初始化 COM（单线程套间）；工厂为共享实例。
        let factory: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let formats = [
            create_format(&factory, TextStyle::Main)?,
            create_format(&factory, TextStyle::Comment)?,
        ];
        // SAFETY: 工厂与格式对象均有效。
        let ellipsis = unsafe {
            [
                factory.CreateEllipsisTrimmingSign(&formats[0])?,
                factory.CreateEllipsisTrimmingSign(&formats[1])?,
            ]
        };
        let params = rendering_params(&factory)?;
        Ok(Self {
            factory,
            formats,
            ellipsis,
            params,
        })
    }

    /// 渲染参数：沿用系统 gamma/对比度/像素几何，模式固定 NATURAL（亚像素定位）。
    pub fn rendering_params(&self) -> &IDWriteRenderingParams {
        &self.params
    }

    fn style_index(style: TextStyle) -> usize {
        match style {
            TextStyle::Main => 0,
            TextStyle::Comment => 1,
        }
    }

    /// 建单行布局（宽度先给上界，调用方随后 [`Self::constrain`]）。
    pub fn new_layout(
        &self,
        text: &str,
        style: TextStyle,
        max_height: f32,
    ) -> Result<IDWriteTextLayout> {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: utf16 在调用期间存活；格式对象属于同一工厂。
        let layout = unsafe {
            self.factory.CreateTextLayout(
                &utf16,
                &self.formats[Self::style_index(style)],
                MEASURE_WIDTH,
                max_height,
            )?
        };
        // SAFETY: 布局对象刚创建且独占。
        unsafe {
            layout.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            layout.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
        }
        Ok(layout)
    }

    /// 收缩到列宽并按字符加省略号（超长词不撑破窗口）。
    pub fn constrain(&self, layout: &IDWriteTextLayout, style: TextStyle, max_width: f32) {
        let trimming = DWRITE_TRIMMING {
            granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
            delimiter: 0,
            delimiterCount: 0,
        };
        // SAFETY: 布局与内联对象均有效；失败只意味着不截断（视觉溢出，绝不 panic）。
        unsafe {
            let _ = layout.SetMaxWidth(max_width.max(0.0));
            let _ = layout.SetTrimming(&trimming, &self.ellipsis[Self::style_index(style)]);
        }
    }

    /// 布局的自然宽度（含尾随空白），单位 DIP。
    pub fn width_of(layout: &IDWriteTextLayout) -> f32 {
        let mut m = DWRITE_TEXT_METRICS::default();
        // SAFETY: 输出结构由 DWrite 填写。
        unsafe {
            let _ = layout.GetMetrics(&mut m);
        }
        m.widthIncludingTrailingWhitespace
    }
}

fn create_format(factory: &IDWriteFactory, style: TextStyle) -> Result<IDWriteTextFormat> {
    let family: Vec<u16> = FONT_FAMILY.encode_utf16().chain([0]).collect();
    let locale: Vec<u16> = "zh-CN".encode_utf16().chain([0]).collect();
    // SAFETY: 两个字符串以 NUL 结尾且存活于调用期间。
    unsafe {
        factory.CreateTextFormat(
            PCWSTR(family.as_ptr()),
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            style.font_size(),
            PCWSTR(locale.as_ptr()),
        )
    }
}

fn rendering_params(factory: &IDWriteFactory) -> Result<IDWriteRenderingParams> {
    // SAFETY: 只读查询系统渲染参数。
    unsafe {
        let base = match factory.CreateRenderingParams() {
            Ok(p) => p,
            // 系统参数不可读（极少见）：给一组保守默认值，仍是 NATURAL。
            Err(_) => return fallback_params(factory),
        };
        factory.CreateCustomRenderingParams(
            base.GetGamma(),
            base.GetEnhancedContrast(),
            base.GetClearTypeLevel(),
            base.GetPixelGeometry(),
            DWRITE_RENDERING_MODE_NATURAL,
        )
    }
}

fn fallback_params(factory: &IDWriteFactory) -> Result<IDWriteRenderingParams> {
    // SAFETY: 纯数值构造，无外部状态。
    unsafe {
        factory.CreateCustomRenderingParams(
            1.8,
            0.5,
            1.0,
            DWRITE_PIXEL_GEOMETRY_FLAT,
            DWRITE_RENDERING_MODE_NATURAL,
        )
    }
}

/// 候选行与组字串的排版结果，供渲染后端按行绘制。
pub struct RowText {
    pub layout: IDWriteTextLayout,
    pub width: f32,
}

impl RowText {
    pub fn measure(engine: &TextEngine, text: &str, style: TextStyle) -> Result<Self> {
        let layout = engine.new_layout(text, style, METRICS.row_height)?;
        let width = TextEngine::width_of(&layout);
        Ok(Self { layout, width })
    }
}
