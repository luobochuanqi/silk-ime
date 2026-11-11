//! 候选窗的纯几何：内容布局与屏幕定位。
//!
//! 这里不触碰任何 Win32/D2D/DirectWrite 类型，也不读系统状态，因此可以
//! 用单元测试覆盖多显示器、负坐标、小屏、空候选等边界（06 的定位规则）。
//! 单位约定：布局用 DIP，定位用屏幕物理像素；换算只发生在 [`dip_to_px`]
//! 与渲染边界。

/// 浮点矩形（DIP），左上角 + 宽高。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    fn right(&self) -> f32 {
        self.x + self.w
    }
}

/// 整数矩形（屏幕物理像素），左上右下。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl IRect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// 由插入点矩形（x, y, w, h）构造；负宽高按 0 处理，避免畸形输入倒置矩形。
    pub fn from_xywh(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self::new(x, y, x.saturating_add(w.max(0)), y.saturating_add(h.max(0)))
    }

    /// 仅测试与诊断使用：定位只需要左上右下。
    #[cfg(test)]
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

/// 字号单级：候选词与序号同一级，注释与组字串降一级（06 字体规则）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextStyle {
    Main,
    Comment,
}

impl TextStyle {
    /// 基准字号（DIP）。选 15 是因为微软雅黑在 96 DPI 下 15px 仍是
    /// 整数字号，ClearType 网格对齐最稳。
    pub const fn font_size(self) -> f32 {
        match self {
            TextStyle::Main => 15.0,
            TextStyle::Comment => 12.0,
        }
    }

    /// 行内可用高度（DIP）：主行按字号 + 上下留白；注释行跟随主行高度以便
    /// 垂直居中同一行基线。
    pub const fn line_height(self) -> f32 {
        match self {
            TextStyle::Main => 24.0,
            TextStyle::Comment => 18.0,
        }
    }
}

/// 布局尺寸常量（DIP）。数值取 Windows 11 系统菜单的观感：窄内边距、
/// 行高 24、列间距 6/10；`max_width` 防止超长候选把窗口拉出屏幕。
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub pad_x: f32,
    pub pad_y: f32,
    pub row_height: f32,
    pub header_height: f32,
    pub gap_index: f32,
    pub gap_comment: f32,
    pub min_width: f32,
    pub max_width: f32,
}

impl Metrics {
    pub const fn standard() -> Self {
        Self {
            pad_x: 10.0,
            pad_y: 6.0,
            row_height: TextStyle::Main.line_height(),
            header_height: TextStyle::Comment.line_height(),
            gap_index: 6.0,
            gap_comment: 12.0,
            min_width: 96.0,
            max_width: 480.0,
        }
    }
}

pub const METRICS: Metrics = Metrics::standard();

impl Default for Metrics {
    fn default() -> Self {
        Self::standard()
    }
}
/// 一行待排版的度量（由排版层从 DirectWrite 取宽度后填入）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RowInput {
    pub index_w: f32,
    pub text_w: f32,
    pub comment_w: f32,
}

/// 排版结果（DIP）。`width`/`height` 是窗口客户区尺寸。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellRow {
    pub cell: RectF,
    pub index: RectF,
    pub text: RectF,
    pub comment: Option<RectF>,
}

/// 整窗布局。
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub width: f32,
    pub height: f32,
    pub preedit: Option<RectF>,
    pub rows: Vec<CellRow>,
}

/// 计算整窗布局：单层纵向候选行（序号 + 词 + 注释列），组字串作为顶部一行。
///
/// 宽度超过 `metrics.max_width` 时窗口封顶，词列优先让位（由排版层按列宽
/// 加省略号），注释列仍尽量保留。
pub fn layout(metrics: &Metrics, preedit_w: Option<f32>, rows: &[RowInput]) -> Layout {
    let pad = metrics.pad_x * 2.0;
    let mut content_w: f32 = 0.0;
    for row in rows {
        content_w = content_w.max(row_width(metrics, row));
    }
    let header_w = preedit_w.unwrap_or(0.0);
    let width = (pad + content_w)
        .max(pad + header_w)
        .max(metrics.min_width)
        .min(metrics.max_width.max(metrics.min_width));

    let header_h = if preedit_w.is_some() {
        metrics.header_height
    } else {
        0.0
    };
    let height = metrics.pad_y * 2.0 + header_h + metrics.row_height * rows.len() as f32;
    let inner_w = width - pad;

    let preedit = preedit_w.map(|_| RectF::new(metrics.pad_x, metrics.pad_y, inner_w, metrics.header_height));

    let mut out = Vec::with_capacity(rows.len());
    let mut y = metrics.pad_y + header_h;
    for row in rows {
        let cell = RectF::new(metrics.pad_x, y, inner_w, metrics.row_height);
        let index = RectF::new(cell.x, y, row.index_w, metrics.row_height);
        let text_x = index.right() + metrics.gap_index;
        let room = (cell.right() - text_x).max(0.0);
        let text_w = row.text_w.min(room);
        let text = RectF::new(text_x, y, text_w, metrics.row_height);
        let comment = if row.comment_w > 0.0 {
            let cx = text.right() + metrics.gap_comment;
            let cw = row.comment_w.min((cell.right() - cx).max(0.0));
            if cw > 0.0 {
                Some(RectF::new(cx, y, cw, metrics.row_height))
            } else {
                None
            }
        } else {
            None
        };
        out.push(CellRow {
            cell,
            index,
            text,
            comment,
        });
        y += metrics.row_height;
    }

    Layout {
        width,
        height,
        preedit,
        rows: out,
    }
}

fn row_width(metrics: &Metrics, row: &RowInput) -> f32 {
    let mut w = row.index_w + metrics.gap_index + row.text_w;
    if row.comment_w > 0.0 {
        w += metrics.gap_comment + row.comment_w;
    }
    w
}

/// 定位结果：窗口左上角（物理像素）与是否翻转到插入点上方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub flipped: bool,
}

/// 06 定位规则：贴插入点下沿 + gap；下方放不下翻上方；水平/垂直钳制在
/// 显示器工作区内。`anchor` 与 `area` 都是屏幕物理像素。
pub fn place(anchor: IRect, size: (i32, i32), gap: i32, area: IRect) -> Placement {
    let (w, h) = (size.0.max(0), size.1.max(0));
    let below = anchor.bottom.saturating_add(gap);
    let above = anchor.top.saturating_sub(gap).saturating_sub(h);
    let flipped = below.saturating_add(h) > area.bottom && above >= area.top;
    let raw_y = if flipped { above } else { below };
    Placement {
        x: clamp(anchor.left, area.left, area.right.saturating_sub(w)),
        y: clamp(raw_y, area.top, area.bottom.saturating_sub(h)),
        flipped,
    }
}

/// 先夹到 `hi` 再夹到 `lo`：窗口比工作区还大时（`hi < lo`）贴住 `lo`，
/// 与 Windows 自身对超大弹出窗的处理一致。
fn clamp(v: i32, lo: i32, hi: i32) -> i32 {
    if hi <= lo {
        lo
    } else {
        v.clamp(lo, hi)
    }
}

/// DIP → 物理像素，四舍五入；只在渲染/建窗边界调用。
pub fn dip_to_px(dip: f32, dpi: u32) -> i32 {
    let scale = dpi as f32 / 96.0;
    (dip * scale).round() as i32
}

/// 物理像素 → DIP（排版与诊断用）。
#[cfg(test)]
pub fn px_to_dip(px: i32, dpi: u32) -> f32 {
    px as f32 * 96.0 / dpi.max(1) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> Metrics {
        Metrics::default()
    }

    fn two_rows() -> Vec<RowInput> {
        vec![
            RowInput {
                index_w: 8.0,
                text_w: 40.0,
                comment_w: 0.0,
            },
            RowInput {
                index_w: 8.0,
                text_w: 60.0,
                comment_w: 30.0,
            },
        ]
    }

    #[test]
    fn layout_empty_candidates_keeps_min_width() {
        let l = layout(&metrics(), None, &[]);
        assert_eq!(l.height, 12.0);
        assert_eq!(l.width, metrics().min_width);
        assert!(l.rows.is_empty());
        assert!(l.preedit.is_none());
    }

    #[test]
    fn layout_rows_stack_and_height_matches_count() {
        let l = layout(&metrics(), None, &two_rows());
        assert_eq!(l.rows.len(), 2);
        assert_eq!(l.rows[0].cell.y, 6.0);
        assert_eq!(l.rows[1].cell.y, 30.0);
        assert_eq!(l.height, 6.0 + 48.0 + 6.0);
    }

    #[test]
    fn layout_wide_row_expands_width() {
        let l = layout(&metrics(), None, &two_rows());
        // pad 20 + 序号 8 + 间隙 6 + 词 60 + 注释间隙 12 + 注释 30
        assert_eq!(l.width, 136.0);
    }

    #[test]
    fn layout_narrow_rows_keep_min_width() {
        let rows = vec![RowInput {
            index_w: 4.0,
            text_w: 10.0,
            comment_w: 0.0,
        }];
        let l = layout(&metrics(), None, &rows);
        assert_eq!(l.width, metrics().min_width);
        // 行框拉伸到内宽，判定框不再溢出窗口
        assert_eq!(l.rows[0].text.x, 20.0);
        assert_eq!(l.rows[0].cell.x + l.rows[0].cell.w, l.width - 10.0);
    }

    #[test]
    fn layout_caps_width_and_clips_columns() {
        // 超长候选：窗口封顶在 max_width，词列与注释列按剩余空间收缩。
        let rows = vec![RowInput {
            index_w: 8.0,
            text_w: 2000.0,
            comment_w: 800.0,
        }];
        let l = layout(&metrics(), None, &rows);
        assert_eq!(l.width, metrics().max_width);
        let row = l.rows[0];
        assert!(row.text.x + row.text.w <= l.width - metrics().pad_x + 0.001);
        // 注释被挤到 0 宽时直接不画，而不是溢出窗口。
        assert!(row.comment.is_none());

        // 中等超长：注释仍有空间则保留。
        let rows = vec![RowInput {
            index_w: 8.0,
            text_w: 300.0,
            comment_w: 60.0,
        }];
        let l = layout(&metrics(), None, &rows);
        assert_eq!(l.width, metrics().max_width.min(20.0 + 8.0 + 6.0 + 300.0 + 12.0 + 60.0));
        assert!(l.rows[0].comment.is_some());
    }

    #[test]
    fn layout_comment_column_follows_text() {
        let l = layout(&metrics(), None, &two_rows());
        let row = l.rows[1];
        let text_right = row.text.x + row.text.w;
        assert_eq!(row.comment.unwrap().x, text_right + 12.0);
        assert!(l.rows[0].comment.is_none());
    }

    #[test]
    fn layout_preedit_adds_header_and_pushes_rows_down() {
        let l = layout(&metrics(), Some(80.0), &two_rows());
        assert_eq!(l.preedit.unwrap().h, 18.0);
        assert_eq!(l.rows[0].cell.y, 6.0 + 18.0);
        assert_eq!(l.height, 6.0 + 18.0 + 48.0 + 6.0);
    }

    #[test]
    fn layout_preedit_only() {
        let l = layout(&metrics(), Some(50.0), &[]);
        assert_eq!(l.height, 6.0 + 18.0 + 6.0);
        assert_eq!(l.width, metrics().min_width);
        assert!(l.rows.is_empty());
        assert!(l.preedit.is_some());
    }

    #[test]
    fn place_below_when_space_available() {
        let area = IRect::new(0, 0, 1920, 1080);
        let anchor = IRect::new(100, 100, 104, 120);
        let p = place(anchor, (300, 200), 4, area);
        assert!(!p.flipped);
        assert_eq!(p.x, 100);
        assert_eq!(p.y, 124);
    }

    #[test]
    fn place_flips_above_when_below_overflows() {
        let area = IRect::new(0, 0, 1920, 1080);
        let anchor = IRect::new(100, 900, 104, 920);
        let p = place(anchor, (300, 200), 4, area);
        assert!(p.flipped);
        assert_eq!(p.y, 900 - 4 - 200);
    }

    #[test]
    fn place_keeps_below_when_neither_side_fits() {
        // 上下都放不下：按 06 优先保证可见，钳制在下方起始位置（工作区内）。
        let area = IRect::new(0, 0, 800, 300);
        let anchor = IRect::new(100, 140, 104, 160);
        let p = place(anchor, (300, 250), 4, area);
        assert!(!p.flipped);
        assert_eq!(p.y, area.bottom - 250);
    }

    #[test]
    fn place_clamps_window_taller_than_work_area() {
        let area = IRect::new(0, 0, 800, 300);
        let anchor = IRect::new(10, 10, 14, 20);
        let p = place(anchor, (300, 400), 4, area);
        assert_eq!(p.y, area.top);
    }

    #[test]
    fn place_clamps_x_at_right_edge() {
        let area = IRect::new(0, 0, 1920, 1080);
        let anchor = IRect::new(1850, 100, 1854, 120);
        let p = place(anchor, (300, 100), 4, area);
        assert_eq!(p.x, 1920 - 300);
    }

    #[test]
    fn place_clamps_x_at_left_edge_of_negative_monitor() {
        // 左侧副屏：工作区 x 为负，插入点贴屏幕左缘。
        let area = IRect::new(-1920, 0, 0, 1080);
        let anchor = IRect::new(-1915, 300, -1911, 320);
        let p = place(anchor, (280, 160), 4, area);
        // 窗口放得下就不夹：左缘贴插入点，右缘仍在副屏内。
        assert_eq!(p.x, -1915);
        assert_eq!(p.y, 324);
    }

    #[test]
    fn place_negative_coordinates_keep_relative_position() {
        let area = IRect::new(-2560, -200, 0, 1240);
        let anchor = IRect::new(-1200, -150, -1196, -130);
        let p = place(anchor, (200, 100), 4, area);
        assert_eq!(p.x, -1200);
        assert_eq!(p.y, -126);
        assert!(!p.flipped);
    }

    #[test]
    fn place_window_wider_than_work_area_pins_left() {
        let area = IRect::new(0, 0, 400, 300);
        let anchor = IRect::new(50, 50, 54, 70);
        let p = place(anchor, (600, 100), 4, area);
        assert_eq!(p.x, 0);
    }

    #[test]
    fn place_anchor_outside_area_still_clamped() {
        let area = IRect::new(0, 0, 1920, 1080);
        let anchor = IRect::new(-50, -300, -46, -280);
        let p = place(anchor, (200, 100), 4, area);
        assert_eq!(p.x, 0);
        assert_eq!(p.y, 0);
        assert!(!p.flipped);
    }

    #[test]
    fn place_small_screen_zero_height() {
        let area = IRect::new(0, 0, 320, 240);
        let anchor = IRect::new(300, 200, 304, 220);
        let p = place(anchor, (100, 0), 4, area);
        // 零高窗口贴下沿，且不越界
        assert_eq!(p.y, 224);
        assert_eq!(p.x, 220);
    }

    #[test]
    fn dip_to_px_rounds_at_common_scales() {
        assert_eq!(dip_to_px(15.0, 96), 15);
        assert_eq!(dip_to_px(15.0, 120), 19); // 150%：18.75 → 19
        assert_eq!(dip_to_px(15.0, 144), 23); // 200%：22.5 → 23（四舍五入）
        assert_eq!(dip_to_px(1.0, 192), 2);
        assert_eq!(px_to_dip(30, 144), 20.0);
    }

    #[test]
    fn irect_from_xywh_clamps_negative_extent() {
        let r = IRect::from_xywh(10, 20, -5, -5);
        assert_eq!(r, IRect::new(10, 20, 10, 20));
        assert_eq!(IRect::from_xywh(-10, -20, 4, 8).height(), 8);
    }
}
