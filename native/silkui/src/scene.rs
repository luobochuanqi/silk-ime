//! 场景：把视图快照排版成可直接绘制的几何 + DirectWrite 布局。
//!
//! 两个渲染后端共用这里产出的场景（06 的"布局与排版代码两后端共享"），
//! 因此绘制只依赖 `ID2D1RenderTarget`——`ID2D1DeviceContext`（主路）与
//! `ID2D1DCRenderTarget`（兜底）都向上转换到它。

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    ID2D1RenderTarget, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE,
};
use windows::Win32::Graphics::DirectWrite::{IDWriteRenderingParams, IDWriteTextLayout};
use windows_numerics::Vector2;

use crate::layout::{self, Layout, Metrics, RectF, RowInput, TextStyle};
use crate::model::Snapshot;
use crate::text::{RowText, TextEngine};
use crate::theme::{Color, Theme};

/// 兜底路自绘阴影的外边距（DIP）；主路由 DWM 承担阴影，无此边距。
pub const SHADOW_MARGIN: f32 = 6.0;
const CORNER_RADIUS: f32 = 8.0;

/// 一行排版后的文本对象。
pub struct SceneRow {
    pub index: IDWriteTextLayout,
    pub text: IDWriteTextLayout,
    pub comment: Option<IDWriteTextLayout>,
}

pub struct Scene {
    pub layout: Layout,
    pub preedit: Option<IDWriteTextLayout>,
    pub rows: Vec<SceneRow>,
    /// 是否包含自绘阴影边距（与窗口形态一致）。
    pub shadow: bool,
}

impl Scene {
    /// 排版快照。`shadow` 决定是否给自绘阴影留边（兜底路）。
    pub fn build(
        engine: &TextEngine,
        snapshot: &Snapshot,
        metrics: &Metrics,
        shadow: bool,
    ) -> Result<Self> {
        let preedit = if snapshot.preedit.is_empty() {
            None
        } else {
            Some(RowText::measure(engine, &snapshot.preedit, TextStyle::Comment)?)
        };

        // 先量自然宽度，再交给纯函数布局（含 max_width 封顶与列裁剪）。
        let mut inputs = Vec::with_capacity(snapshot.rows.len());
        let mut measured = Vec::with_capacity(snapshot.rows.len());
        for (i, row) in snapshot.rows.iter().enumerate() {
            let index = RowText::measure(engine, &format!("{}.", i + 1), TextStyle::Comment)?;
            let text = RowText::measure(engine, &row.text, TextStyle::Main)?;
            let comment = if row.comment.is_empty() {
                None
            } else {
                Some(RowText::measure(engine, &row.comment, TextStyle::Comment)?)
            };
            inputs.push(RowInput {
                index_w: index.width,
                text_w: text.width,
                comment_w: comment.as_ref().map_or(0.0, |c| c.width),
            });
            measured.push((index, text, comment));
        }

        let layout = layout::layout(metrics, preedit.as_ref().map(|p| p.width), &inputs);

        let rows = measured
            .into_iter()
            .zip(&layout.rows)
            .map(|((index, text, comment), box_)| {
                engine.constrain(&index.layout, TextStyle::Comment, box_.index.w);
                engine.constrain(&text.layout, TextStyle::Main, box_.text.w);
                // 注释列被裁到 0 宽（行过长撞上 max_width）时不画注释，但行本身必须
                // 保留：行序与 `layout.rows` 一一对应，paint 依赖这个对齐。
                let comment = match (&comment, box_.comment) {
                    (Some(c), Some(cb)) => {
                        engine.constrain(&c.layout, TextStyle::Comment, cb.w);
                        Some(c.layout.clone())
                    }
                    _ => None,
                };
                Ok(SceneRow {
                    index: index.layout,
                    text: text.layout,
                    comment,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            layout,
            preedit: preedit.map(|p| p.layout),
            rows,
            shadow,
        })
    }

    /// 窗口内容尺寸（DIP），含阴影边距。
    pub fn extent_dip(&self) -> (f32, f32) {
        let margin = self.margin();
        (
            self.layout.width + margin * 2.0,
            self.layout.height + margin * 2.0,
        )
    }

    /// 窗口像素尺寸：DPI 换算只发生在这一边界。
    pub fn extent_px(&self, dpi: u32) -> (i32, i32) {
        let (w, h) = self.extent_dip();
        (
            layout::dip_to_px(w, dpi).max(1),
            layout::dip_to_px(h, dpi).max(1),
        )
    }

    fn margin(&self) -> f32 {
        if self.shadow {
            SHADOW_MARGIN
        } else {
            0.0
        }
    }

    /// 把场景画到目标上；调用方负责 `BeginDraw`/`EndDraw` 与呈现。
    ///
    /// `antialias`：主路 CLEARTYPE（不透明目标），兜底路 GRAYSCALE（alpha DIB）。
    pub fn paint(
        &self,
        target: &ID2D1RenderTarget,
        theme: &Theme,
        antialias: D2D1_TEXT_ANTIALIAS_MODE,
        text_params: &IDWriteRenderingParams,
    ) -> Result<()> {
        let margin = self.margin();
        let panel = RectF::new(margin, margin, self.layout.width, self.layout.height);

        // SAFETY: 目标处于 BeginDraw 状态，Set/Clear/填充均在 EndDraw 之前。
        unsafe {
            target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            target.SetTextAntialiasMode(antialias);
            // NATURAL 亚像素定位（06）：系统 gamma/对比度不变，只固定渲染模式。
            target.SetTextRenderingParams(text_params);
            // 分层窗口需要整块清成透明，圆角外区域才不挡下层像素。
            let clear = if self.shadow {
                D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }
            } else {
                to_d2d(theme.background)
            };
            target.Clear(Some(&clear));
        }

        if self.shadow {
            self.paint_shadow(target, theme)?;
        }

        let bg = brush(target, theme.background)?;
        let rounded = D2D1_ROUNDED_RECT {
            rect: d2d_rect(panel),
            radiusX: CORNER_RADIUS,
            radiusY: CORNER_RADIUS,
        };
        // SAFETY: 目标有效；画刷由同一目标创建。
        unsafe { target.FillRoundedRectangle(&rounded, &bg) };

        let text_brush = brush(target, theme.text)?;
        let dim_brush = brush(target, theme.comment)?;

        if let (Some(preedit), Some(box_)) = (&self.preedit, self.layout.preedit) {
            let rect = offset(box_, margin);
            // SAFETY: 布局由 DWrite 创建，画刷属于同一目标。
            unsafe {
                target.DrawTextLayout(
                    Vector2 {
                        X: rect.x,
                        Y: rect.y,
                    },
                    preedit,
                    &dim_brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
            let line = brush(target, theme.separator)?;
            let sep = RectF::new(rect.x, rect.y + rect.h - 1.0, rect.w, 1.0);
            // SAFETY: 同上。
            unsafe { target.FillRectangle(&d2d_rect(sep), &line) };
        }

        for (row, box_) in self.rows.iter().zip(&self.layout.rows) {
            let idx = offset(box_.index, margin);
            let txt = offset(box_.text, margin);
            // SAFETY: 所有布局与画刷均有效。
            unsafe {
                target.DrawTextLayout(
                    Vector2 {
                        X: idx.x,
                        Y: idx.y,
                    },
                    &row.index,
                    &dim_brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
                target.DrawTextLayout(
                    Vector2 {
                        X: txt.x,
                        Y: txt.y,
                    },
                    &row.text,
                    &text_brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
            if let (Some(comment), Some(cbox)) = (&row.comment, box_.comment) {
                let c = offset(cbox, margin);
                // SAFETY: 同上。
                unsafe {
                    target.DrawTextLayout(
                        Vector2 { X: c.x, Y: c.y },
                        comment,
                        &dim_brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                }
            }
        }

        if self.shadow {
            let border = brush(target, theme.border)?;
            // SAFETY: 同上。
            unsafe { target.DrawRoundedRectangle(&rounded, &border, 1.0, None) };
        }
        Ok(())
    }

    /// 自绘阴影：三圈由内到外递减的不透明度圆环。分层窗口拿不到系统阴影
    /// （DWM 官方约束），这是兜底路的观感补偿。
    fn paint_shadow(&self, target: &ID2D1RenderTarget, theme: &Theme) -> Result<()> {
        let step = SHADOW_MARGIN / 3.0;
        let tint = if theme.dark { 1.0 } else { 0.0 };
        for ring in 1..=3 {
            let grow = step * ring as f32;
            let alpha = 0.10 * (4 - ring) as f32;
            let shadow = brush(target, Color { r: tint, g: tint, b: tint, a: alpha })?;
            let rect = D2D1_ROUNDED_RECT {
                rect: d2d_rect(RectF::new(
                    SHADOW_MARGIN - grow,
                    SHADOW_MARGIN - grow,
                    self.layout.width + grow * 2.0,
                    self.layout.height + grow * 2.0,
                )),
                radiusX: CORNER_RADIUS + grow,
                radiusY: CORNER_RADIUS + grow,
            };
            // SAFETY: 目标有效。
            unsafe { target.FillRoundedRectangle(&rect, &shadow) };
        }
        Ok(())
    }
}

fn brush(target: &ID2D1RenderTarget, color: Color) -> Result<windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush> {
    let c = to_d2d(color);
    // SAFETY: 目标有效；画刷生命周期由返回值持有。
    unsafe { target.CreateSolidColorBrush(&c, None) }
}

pub fn to_d2d(c: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
}

fn d2d_rect(r: RectF) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.x,
        top: r.y,
        right: r.x + r.w,
        bottom: r.y + r.h,
    }
}

fn offset(r: RectF, margin: f32) -> RectF {
    RectF::new(r.x + margin, r.y + margin, r.w, r.h)
}
