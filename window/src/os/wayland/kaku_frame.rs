//! A client-side decoration frame themed with Kaku's own configuration.
//!
//! The structure and protocol handling (subsurfaces, hit testing, geometry,
//! the `DecorationsFrame` implementation) are derived from
//! smithay-client-toolkit's `fallback_frame`, which is Copyright (c) Kompf
//! and contributors and provided under the terms of the Apache-2.0 OR MIT
//! licenses.
//!
//! The theming (colors from `config.window_frame`, the title shaped with the
//! terminal's title font, hover-highlighted buttons, the header separator)
//! is derived from Kaku's earlier ConceptFrame implementation, which was in
//! turn derived from SCTK 0.11's ConceptFrame (Copyright (c) 2018 Victor
//! Berger, MIT).

use std::error::Error;
use std::mem;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use config::RgbaColor;
use smithay_client_toolkit::compositor::SurfaceData;
use smithay_client_toolkit::reexports::client::protocol::wl_shm;
use smithay_client_toolkit::reexports::client::protocol::wl_subsurface::WlSubsurface;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Dispatch, Proxy, QueueHandle};
use smithay_client_toolkit::reexports::csd_frame::{
    CursorIcon, DecorationsFrame, FrameAction, FrameClick, ResizeEdge, WindowManagerCapabilities,
    WindowState,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::Shm;
use smithay_client_toolkit::subcompositor::{SubcompositorState, SubsurfaceData};
use tiny_skia::{ColorU8, PixmapMut, PixmapPaint, PixmapRef, Transform};
use wayland_backend::client::ObjectId;
use wezterm_font::{FontConfiguration, FontMetrics, GlyphInfo, RasterizedGlyph};

/// The size of the header bar.
const HEADER_SIZE: u32 = 36;
const BUTTON_WIDTH: u32 = 34;
const BUTTON_MARGIN: u32 = 6;

/// The size of the border.
const BORDER_SIZE: u32 = 6;

const HEADER: usize = 0;
const TOP_BORDER: usize = 1;
const RIGHT_BORDER: usize = 2;
const BOTTOM_BORDER: usize = 3;
const LEFT_BORDER: usize = 4;

/// The decorated layout of the five frame parts around a `width`x`height`
/// main surface: `(pos, width, height)` per part, in surface-local logical
/// coordinates. This is the single source of truth for the part geometry:
/// `FrameRenderData::new` and the non-fullscreen `resize()` path both come
/// here, so leaving fullscreen restores exactly the positions and sizes the
/// parts were created with.
fn decorated_part_geometry(width: u32, height: u32) -> [((i32, i32), u32, u32); 5] {
    let border = BORDER_SIZE as i32;
    let header = HEADER_SIZE as i32;
    [
        // Header: full width, sitting directly above the main surface.
        ((0, -header), width, HEADER_SIZE),
        // Top border: a header-height above the main surface, overlapping
        // the side borders by BORDER_SIZE on each side.
        ((-border, -header - border), width + 2 * BORDER_SIZE, BORDER_SIZE),
        // Right border: right of the main surface, spanning the header.
        ((width as i32, -header), BORDER_SIZE, height + HEADER_SIZE),
        // Bottom border: below the main surface, spanning the side borders.
        ((-border, height as i32), width + 2 * BORDER_SIZE, BORDER_SIZE),
        // Left border: left of the main surface, spanning the header.
        ((-border, -header), BORDER_SIZE, height + HEADER_SIZE),
    ]
}

/// While fullscreen nothing is drawn, but the compositor derives the
/// window's extent from the whole surface tree: parts left at their
/// decorated positions stick out past the screen and make KWin scale the
/// window down to fit, pushing the bottom rows below the screen edge.
/// Collapsing every part to 1x1 at the origin keeps the tree's extent
/// equal to the main surface.
fn fullscreen_part_geometry() -> [((i32, i32), u32, u32); 5] {
    [((0, 0), 1, 1); 5]
}

/// The frame state, themable through `config.window_frame`.
pub struct KakuFrame<State> {
    /// The parent surface.
    parent: WlSurface,

    /// The latest window state.
    state: WindowState,

    /// The wm capabilities.
    wm_capabilities: WindowManagerCapabilities,

    /// Whether the frame is resizable.
    resizable: bool,

    /// Whether the frame is waiting for redraw.
    dirty: bool,

    /// The location of the mouse.
    mouse_location: Location,

    /// The location of the mouse.
    mouse_coords: (i32, i32),

    /// The frame rendering data. When `None` the frame is hidden.
    render_data: Option<FrameRenderData>,

    /// Whether the frame should sync with the parent.
    ///
    /// This should happen in reaction to scale or resize changes.
    should_sync: bool,

    /// The active scale factor of the frame.
    scale_factor: f64,

    /// The frame queue handle.
    queue_handle: QueueHandle<State>,

    /// The memory pool to use for drawing.
    pool: SlotPool,

    /// The subcompositor.
    subcompositor: Arc<SubcompositorState>,

    /// Buttons state.
    buttons: [Option<UIButton>; 3],
    pressed_button: Option<UIButton>,

    /// The terminal font configuration, used to shape the title.
    font_config: Option<Rc<FontConfiguration>>,

    /// The window title.
    title: String,

    /// The shaped title, cached until the title, state or dpi changes.
    shaped_title: Option<ShapedTitle>,
}

struct ShapedTitle {
    title: String,
    glyphs: Vec<ShapedGlyph>,
    /// The descender in header-buffer pixels, already scaled.
    descender: f32,
    state: WindowState,
    dpi: usize,
    scale: u32,
}

/// A glyph scaled to the header buffer's pixel space, ready to blit.
struct ShapedGlyph {
    /// The scaled, tinted bitmap in premultiplied BGRA (the SHM layout).
    data: Vec<u8>,
    width: u32,
    height: u32,
    x_offset: f32,
    bearing_x: f32,
    bearing_y: f32,
    y_offset: f32,
    x_advance: f32,
}

impl<State> KakuFrame<State>
where
    State: Dispatch<WlSurface, SurfaceData> + Dispatch<WlSubsurface, SubsurfaceData> + 'static,
{
    /// Force a redraw on the next refresh, e.g. after the config (and with
    /// it the window_frame theme) changed.
    pub fn set_dirty(&mut self) {
        self.shaped_title = None;
        self.dirty = true;
    }

    pub fn new(
        parent: &impl WaylandSurface,
        shm: &Shm,
        subcompositor: Arc<SubcompositorState>,
        queue_handle: QueueHandle<State>,
        font_config: Option<Rc<FontConfiguration>>,
    ) -> Result<Self, Box<dyn Error>> {
        let parent = parent.wl_surface().clone();
        let pool = SlotPool::new(1, shm)?;
        let render_data = Some(FrameRenderData::new(&parent, &subcompositor, &queue_handle));

        let wm_capabilities = WindowManagerCapabilities::all();
        Ok(Self {
            parent,
            resizable: true,
            state: WindowState::empty(),
            wm_capabilities,
            dirty: true,
            scale_factor: 1.,
            pool,
            should_sync: true,
            queue_handle,
            subcompositor,
            render_data,
            mouse_location: Location::None,
            mouse_coords: (0, 0),
            buttons: Self::supported_buttons(wm_capabilities),
            pressed_button: None,
            font_config,
            title: String::new(),
            shaped_title: None,
        })
    }

    fn supported_buttons(wm_capabilities: WindowManagerCapabilities) -> [Option<UIButton>; 3] {
        let maximize = wm_capabilities
            .contains(WindowManagerCapabilities::MAXIMIZE)
            .then_some(UIButton::Maximize);
        let minimize = wm_capabilities
            .contains(WindowManagerCapabilities::MINIMIZE)
            .then_some(UIButton::Minimize);
        [Some(UIButton::Close), maximize, minimize]
    }

    fn precise_location(
        buttons: &[Option<UIButton>],
        old: Location,
        width: u32,
        x: f64,
        y: f64,
    ) -> Location {
        match old {
            Location::Head | Location::Button(_) => Self::find_button(buttons, x, y, width),

            Location::Top | Location::TopLeft | Location::TopRight => {
                if x <= f64::from(BORDER_SIZE) {
                    Location::TopLeft
                } else if x >= f64::from(width - BORDER_SIZE) {
                    Location::TopRight
                } else {
                    Location::Top
                }
            }

            Location::Bottom | Location::BottomLeft | Location::BottomRight => {
                if x <= f64::from(BORDER_SIZE) {
                    Location::BottomLeft
                } else if x >= f64::from(width - BORDER_SIZE) {
                    Location::BottomRight
                } else {
                    Location::Bottom
                }
            }

            other => other,
        }
    }

    fn find_button(buttons: &[Option<UIButton>], x: f64, y: f64, w: u32) -> Location {
        hit_button(buttons, x, y, w).map_or(Location::Head, Location::Button)
    }

    #[inline]
    fn part_index_for_surface(&mut self, surface_id: &ObjectId) -> Option<usize> {
        self.render_data
            .as_ref()?
            .parts
            .iter()
            .position(|part| &part.surface.id() == surface_id)
    }

    /// Shape the title with the terminal's title font, tinting monochrome
    /// glyphs with the active titlebar foreground color. Ported from the
    /// Shape the title with the terminal's own font and scale every glyph
    /// into the header's own pixel space, so the titlebar matches the
    /// terminal's typeface.
    ///
    /// The font stack sizes fonts from the *terminal's* dpi, which also
    /// carries the terminal font scale; the header buffer instead lives in
    /// `HEADER_SIZE * surface_scale` pixels. On macOS those coincide, but
    /// elsewhere they can differ by 2x or more, which once drew the glyphs
    /// twice as tall as the header and mostly above the canvas. Scaling the
    /// rasterized bitmaps here makes the drawn title a fixed fraction of the
    /// header height regardless of the terminal's dpi.
    fn reshape_title(&mut self) {
        let Some(font_config) = self.font_config.clone() else {
            return;
        };
        if self.title.is_empty() {
            self.shaped_title = None;
            return;
        }

        let scale = self.scale_factor.ceil() as u32;
        if let Some(existing) = self.shaped_title.as_ref() {
            if existing.title == self.title
                && existing.state == self.state
                && existing.dpi == font_config.get_dpi()
                && existing.scale == scale
            {
                return;
            }
        }

        // Use the terminal's primary font (with its fallback chain), not
        // the window_frame title font, so the titlebar is set in the same
        // typeface as the terminal content.
        let font = match font_config.default_font() {
            Ok(font) => font,
            Err(err) => {
                log::warn!("unable to load font for the title: {err:#}");
                self.shaped_title = None;
                return;
            }
        };
        let metrics = font.metrics();
        let infos = match font.shape(
            &self.title,
            || {},
            |_| {},
            None,
            wezterm_bidi::Direction::LeftToRight,
            None,
            None,
        ) {
            Ok(infos) => infos,
            Err(err) => {
                log::warn!("unable to shape title: {err:#}");
                self.shaped_title = None;
                return;
            }
        };

        let colors = themed_colors();
        let title_color = if self.state.contains(WindowState::ACTIVATED) {
            colors.active_fg
        } else {
            colors.inactive_fg
        };

        // Aim the font's cell height at ~60% of the header bar, scaled by
        // the user's window_frame.font_size preference (12 is the default),
        // and clamped so an oversized preference cannot leave the header.
        let font_config_handle = font_config.config();
        let size_factor = font_config_handle.window_frame.font_size.unwrap_or(12.) as f32 / 12.;
        let max_cell_px = HEADER_SIZE as f32 * scale as f32 * 0.75;
        let target_px = (18. * scale as f32 * size_factor).min(max_cell_px);
        let cell_px = metrics.cell_height.get() as f32;
        let ratio = if cell_px > 0. {
            target_px / cell_px
        } else {
            1.
        };

        let mut glyphs = vec![];
        for info in infos {
            if let Ok(glyph) = font.rasterize_glyph(info.glyph_pos, info.font_idx) {
                // Work on a copy: the rasterizer may cache the glyph, and we
                // rewrite the pixels below.
                let mut data = glyph.data.clone();
                // Fixup colors: they need to be switched to the SHM pixel
                // format, and monochrome font data must be tinted with the
                // title color. `RgbaColor` derefs to `SrgbaTuple`, whose
                // `.0`/`.1`/`.2` are the straight-alpha float components.
                if let Some(mut pixmap) =
                    PixmapMut::from_bytes(&mut data, glyph.width as u32, glyph.height as u32)
                {
                    for p in pixmap.pixels_mut() {
                        let c = p.demultiply();
                        let (r, g, b, a) = (c.red(), c.green(), c.blue(), c.alpha());
                        if glyph.has_color {
                            *p = ColorU8::from_rgba(b, g, r, a).premultiply();
                        } else {
                            *p = ColorU8::from_rgba(
                                ((b as f32 / 255.) * (title_color.0 * 255.)).min(255.) as u8,
                                ((g as f32 / 255.) * (title_color.1 * 255.)).min(255.) as u8,
                                ((r as f32 / 255.) * (title_color.2 * 255.)).min(255.) as u8,
                                a,
                            )
                            .premultiply();
                        }
                    }
                }

                // Scale the bitmap into the header's pixel space.
                let scaled_w = ((glyph.width as f32) * ratio).ceil().max(1.) as u32;
                let scaled_h = ((glyph.height as f32) * ratio).ceil().max(1.) as u32;
                let mut scaled = match tiny_skia::Pixmap::new(scaled_w, scaled_h) {
                    Some(p) => p,
                    None => continue,
                };
                if let Some(src) =
                    PixmapRef::from_bytes(&data, glyph.width as u32, glyph.height as u32)
                {
                    let paint = PixmapPaint {
                        quality: tiny_skia::FilterQuality::Bilinear,
                        ..Default::default()
                    };
                    scaled.draw_pixmap(
                        0,
                        0,
                        src,
                        &paint,
                        tiny_skia::Transform::from_scale(ratio, ratio),
                        None,
                    );
                }

                glyphs.push(ShapedGlyph {
                    data: scaled.take(),
                    width: scaled_w,
                    height: scaled_h,
                    x_offset: info.x_offset.get() as f32 * ratio,
                    bearing_x: glyph.bearing_x.get() as f32 * ratio,
                    bearing_y: glyph.bearing_y.get() as f32 * ratio,
                    y_offset: info.y_offset.get() as f32 * ratio,
                    x_advance: info.x_advance.get() as f32 * ratio,
                });
            }
        }

        self.shaped_title = Some(ShapedTitle {
            title: self.title.clone(),
            glyphs,
            descender: metrics.descender.get() as f32 * ratio,
            state: self.state,
            dpi: font_config.get_dpi(),
            scale,
        });
    }

    /// Draw the pre-scaled title glyphs into the header canvas, horizontally
    /// centered in the region left of the buttons: baseline at 3/4 of the
    /// header height, adjusted by the (already scaled) descender and
    /// per-glyph offsets. All glyph metrics are in header-buffer pixels.
    fn draw_title(
        canvas: &mut [u8],
        width: u32,
        height: u32,
        scale: u32,
        shaped: &ShapedTitle,
        button_count: usize,
    ) {
        let Some(mut pixmap) = PixmapMut::from_bytes(canvas, width * scale, height * scale) else {
            return;
        };
        let scale_f = scale as f32;
        // The buttons occupy the rightmost button-count header cells; keep a
        // symmetric 8px margin on both sides of the usable title region.
        let left_margin = 8. * scale_f;
        let right_reserve =
            (button_count as f32 * BUTTON_WIDTH as f32 + BUTTON_MARGIN as f32 + 8.) * scale_f;
        let usable = (width as f32 * scale_f - left_margin - right_reserve).max(0.);
        // Measure how much of the title fits, then center it in the region.
        let mut fitted = 0f32;
        for item in &shaped.glyphs {
            if fitted + item.x_advance > usable {
                break;
            }
            fitted += item.x_advance;
        }
        let mut x: f32 = left_margin + (usable - fitted) / 2.;
        let paint = PixmapPaint::default();
        for item in &shaped.glyphs {
            if x + item.x_advance > left_margin + usable {
                break;
            }
            let gx = x + item.x_offset + item.bearing_x;
            if gx + item.width as f32 > left_margin + usable {
                break;
            }
            let gy = height as f32 * scale_f * 3. / 4. + shaped.descender
                - (item.y_offset + item.bearing_y);
            if std::env::var_os("KAKU_FRAME_DEBUG").is_some() {
                log::warn!(
                    "kaku-frame: glyph canvas {}x{} scale {} draw_at ({}, {}) gw={} gh={}",
                    width,
                    height,
                    scale,
                    gx,
                    gy,
                    item.width,
                    item.height
                );
            }
            if let Some(data) = PixmapRef::from_bytes(&item.data, item.width, item.height) {
                pixmap.draw_pixmap(
                    gx as i32,
                    gy as i32,
                    data,
                    &paint,
                    Transform::identity(),
                    None,
                );
            }

            x += item.x_advance;
            if x >= left_margin + usable {
                // Don't overflow the buttons
                break;
            }
        }
    }
}

fn button_left(width: u32, index: usize) -> Option<u32> {
    width.checked_sub(BUTTON_MARGIN + (index as u32 + 1) * BUTTON_WIDTH)
}

fn hit_button(buttons: &[Option<UIButton>], x: f64, y: f64, width: u32) -> Option<UIButton> {
    if !(0. ..HEADER_SIZE as f64).contains(&y) {
        return None;
    }
    buttons
        .iter()
        .flatten()
        .enumerate()
        .find_map(|(index, &button)| {
            let left = button_left(width, index)? as f64;
            (x >= left && x < left + BUTTON_WIDTH as f64).then_some(button)
        })
}

fn released_button(pressed: &mut Option<UIButton>, location: Location) -> Option<UIButton> {
    let previous = pressed.take()?;
    (location == Location::Button(previous)).then_some(previous)
}

fn draw_buttons(
    buttons: &[Option<UIButton>],
    canvas: &mut [u8],
    width: u32,
    height: u32,
    scale: u32,
    state: WindowState,
    location: Location,
    pressed: Option<UIButton>,
    colors: &ThemedColors,
) {
    use tiny_skia::{LineCap, Paint, PathBuilder, Stroke};
    let Some(mut pixmap) = PixmapMut::from_bytes(canvas, width * scale, height * scale) else {
        return;
    };
    let transform = Transform::from_scale(scale as f32, scale as f32);
    let active = state.contains(WindowState::ACTIVATED);
    let color_paint = |color: RgbaColor| {
        let (red, green, blue, alpha) = color.as_rgba_u8();
        let mut paint = Paint::default();
        paint.set_color_rgba8(blue, green, red, alpha);
        paint
    };
    let separator = if active {
        colors.active_separator
    } else {
        colors.inactive_separator
    };
    if let Some(rect) = tiny_skia::Rect::from_xywh(0., height as f32 - 1., width as f32, 1.) {
        pixmap.fill_rect(rect, &color_paint(separator), transform, None);
    }
    for (index, &button) in buttons.iter().flatten().enumerate() {
        let Some(left) = button_left(width, index) else {
            continue;
        };
        let center_x = left as f32 + BUTTON_WIDTH as f32 / 2.;
        let center_y = HEADER_SIZE as f32 / 2.;
        let hovered = location == Location::Button(button);
        let foreground = if hovered {
            colors.button_hover_fg
        } else if active {
            colors.active_fg
        } else {
            colors.inactive_fg
        };
        let (red, green, blue, _) = foreground.as_rgba_u8();
        let mut paint = Paint::default();
        paint.set_color_rgba8(blue, green, red, if active { 24 } else { 12 });
        if hovered {
            paint = if button == UIButton::Close {
                let mut danger = Paint::default();
                danger.set_color_rgba8(81, 72, 220, 255);
                danger
            } else {
                color_paint(colors.button_hover_bg)
            };
        }
        if let Some(circle) = PathBuilder::from_circle(
            center_x,
            center_y,
            if pressed == Some(button) && hovered {
                11.
            } else {
                12.
            },
        ) {
            pixmap.fill_path(
                &circle,
                &paint,
                tiny_skia::FillRule::Winding,
                transform,
                None,
            );
        }
        let mut path = PathBuilder::new();
        match button {
            UIButton::Close => {
                path.move_to(center_x - 3.5, center_y - 3.5);
                path.line_to(center_x + 3.5, center_y + 3.5);
                path.move_to(center_x + 3.5, center_y - 3.5);
                path.line_to(center_x - 3.5, center_y + 3.5);
            }
            UIButton::Minimize => {
                path.move_to(center_x - 4., center_y);
                path.line_to(center_x + 4., center_y);
            }
            UIButton::Maximize => {
                if state.contains(WindowState::MAXIMIZED) {
                    path.move_to(center_x - 1., center_y - 4.);
                    path.line_to(center_x + 4., center_y - 4.);
                    path.line_to(center_x + 4., center_y + 1.);
                    path.move_to(center_x - 4., center_y - 1.);
                    path.line_to(center_x + 1., center_y - 1.);
                    path.line_to(center_x + 1., center_y + 4.);
                    path.line_to(center_x - 4., center_y + 4.);
                } else {
                    path.move_to(center_x - 3.5, center_y - 3.5);
                    path.line_to(center_x + 3.5, center_y - 3.5);
                    path.line_to(center_x + 3.5, center_y + 3.5);
                    path.line_to(center_x - 3.5, center_y + 3.5);
                }
                path.close();
            }
        }
        if let Some(path) = path.finish() {
            let mut ink = color_paint(foreground);
            if hovered && button == UIButton::Close {
                ink.set_color_rgba8(255, 255, 255, 255);
            }
            pixmap.stroke_path(
                &path,
                &ink,
                &Stroke {
                    width: 1.35,
                    line_cap: LineCap::Round,
                    ..Stroke::default()
                },
                transform,
                None,
            );
        }
    }
}

/// The theme colors for one draw, from `config.window_frame`.
struct ThemedColors {
    active_bg: RgbaColor,
    inactive_bg: RgbaColor,
    active_fg: RgbaColor,
    inactive_fg: RgbaColor,
    active_separator: RgbaColor,
    inactive_separator: RgbaColor,
    button_hover_bg: RgbaColor,
    button_hover_fg: RgbaColor,
}

fn themed_colors() -> ThemedColors {
    let frame = &config::configuration().window_frame;
    ThemedColors {
        active_bg: frame.active_titlebar_bg,
        inactive_bg: frame.inactive_titlebar_bg,
        active_fg: frame.active_titlebar_fg,
        inactive_fg: frame.inactive_titlebar_fg,
        active_separator: frame.active_titlebar_border_bottom,
        inactive_separator: frame.inactive_titlebar_border_bottom,
        button_hover_bg: frame.button_hover_bg,
        button_hover_fg: frame.button_hover_fg,
    }
}

/// Convert a straight-alpha color to premultiplied BGRA bytes, the SHM
/// Argb8888 pixel layout.
fn premultiply(c: RgbaColor) -> [u8; 4] {
    let (r, g, b, a) = c.as_rgba_u8();
    let alpha = a as f32 / 255.;
    [
        ((b as f32) * alpha) as u8,
        ((g as f32) * alpha) as u8,
        ((r as f32) * alpha) as u8,
        a,
    ]
}

impl<State> DecorationsFrame for KakuFrame<State>
where
    State: Dispatch<WlSurface, SurfaceData> + Dispatch<WlSubsurface, SubsurfaceData> + 'static,
{
    fn set_scaling_factor(&mut self, scale_factor: f64) {
        self.scale_factor = scale_factor;
        self.dirty = true;
        self.should_sync = true;
    }

    fn on_click(
        &mut self,
        _timestamp: Duration,
        click: FrameClick,
        pressed: bool,
    ) -> Option<FrameAction> {
        // Handle alternate click before everything else.
        if click == FrameClick::Alternate {
            return if Location::Head != self.mouse_location
                || !self
                    .wm_capabilities
                    .contains(WindowManagerCapabilities::WINDOW_MENU)
            {
                None
            } else {
                Some(FrameAction::ShowMenu(
                    self.mouse_coords.0,
                    self.mouse_coords.1 - HEADER_SIZE as i32,
                ))
            };
        }

        if click != FrameClick::Normal {
            return None;
        }
        self.dirty = true;
        if pressed {
            self.pressed_button = match self.mouse_location {
                Location::Button(button) => Some(button),
                _ => None,
            };
        } else {
            let button = released_button(&mut self.pressed_button, self.mouse_location)?;
            return match button {
                UIButton::Close => Some(FrameAction::Close),
                UIButton::Minimize => Some(FrameAction::Minimize),
                UIButton::Maximize if self.resizable => {
                    Some(if self.state.contains(WindowState::MAXIMIZED) {
                        FrameAction::UnMaximize
                    } else {
                        FrameAction::Maximize
                    })
                }
                _ => None,
            };
        }
        let resize = pressed && self.resizable;
        match self.mouse_location {
            Location::Head if pressed => Some(FrameAction::Move),
            Location::Top if resize => Some(FrameAction::Resize(ResizeEdge::Top)),
            Location::TopLeft if resize => Some(FrameAction::Resize(ResizeEdge::TopLeft)),
            Location::Left if resize => Some(FrameAction::Resize(ResizeEdge::Left)),
            Location::BottomLeft if resize => Some(FrameAction::Resize(ResizeEdge::BottomLeft)),
            Location::Bottom if resize => Some(FrameAction::Resize(ResizeEdge::Bottom)),
            Location::BottomRight if resize => Some(FrameAction::Resize(ResizeEdge::BottomRight)),
            Location::Right if resize => Some(FrameAction::Resize(ResizeEdge::Right)),
            Location::TopRight if resize => Some(FrameAction::Resize(ResizeEdge::TopRight)),
            _ => None,
        }
    }

    fn click_point_moved(
        &mut self,
        _timestamp: Duration,
        surface_id: &ObjectId,
        x: f64,
        y: f64,
    ) -> Option<CursorIcon> {
        let part_index = self.part_index_for_surface(surface_id)?;
        let location = match part_index {
            LEFT_BORDER => Location::Left,
            RIGHT_BORDER => Location::Right,
            BOTTOM_BORDER => Location::Bottom,
            TOP_BORDER => Location::Top,
            _ => Location::Head,
        };

        let old_location = self.mouse_location;
        self.mouse_coords = (x as i32, y as i32);
        self.mouse_location = Self::precise_location(
            &self.buttons,
            location,
            self.render_data.as_ref().unwrap().parts[part_index].width,
            x,
            y,
        );

        // Set dirty if we moved the cursor between the buttons.
        self.dirty |= (matches!(old_location, Location::Button(_))
            || matches!(self.mouse_location, Location::Button(_)))
            && old_location != self.mouse_location;

        Some(match self.mouse_location {
            Location::Top => CursorIcon::NResize,
            Location::TopRight => CursorIcon::NeResize,
            Location::Right => CursorIcon::EResize,
            Location::BottomRight => CursorIcon::SeResize,
            Location::Bottom => CursorIcon::SResize,
            Location::BottomLeft => CursorIcon::SwResize,
            Location::Left => CursorIcon::WResize,
            Location::TopLeft => CursorIcon::NwResize,
            _ => CursorIcon::Default,
        })
    }

    fn click_point_left(&mut self) {
        self.mouse_location = Location::None;
        self.pressed_button = None;
        self.dirty = true;
    }

    fn set_hidden(&mut self, hidden: bool) {
        if self.is_hidden() == hidden {
            return;
        }

        if hidden {
            self.render_data = None;
        } else {
            let _ = self.pool.resize(1);
            self.render_data = Some(FrameRenderData::new(
                &self.parent,
                &self.subcompositor,
                &self.queue_handle,
            ));
        }
    }

    fn set_resizable(&mut self, resizable: bool) {
        self.resizable = resizable;
        self.buttons = Self::supported_buttons(self.wm_capabilities);
        if !resizable {
            self.buttons[1] = None;
        }
        self.pressed_button = None;
        self.dirty = true;
    }

    fn update_state(&mut self, state: WindowState) {
        let difference = self.state.symmetric_difference(state);
        self.state = state;
        self.dirty |= !difference
            .intersection(WindowState::ACTIVATED | WindowState::FULLSCREEN | WindowState::MAXIMIZED)
            .is_empty();
    }

    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) {
        let parts = &mut self
            .render_data
            .as_mut()
            .expect("trying to resize hidden frame")
            .parts;

        let width = width.get();
        let height = height.get();

        let geometry = if self.state.contains(WindowState::FULLSCREEN) {
            fullscreen_part_geometry()
        } else {
            decorated_part_geometry(width, height)
        };
        for (part, (pos, width, height)) in parts.iter_mut().zip(geometry) {
            part.pos = pos;
            part.width = width;
            part.height = height;
        }

        self.dirty = true;
        self.should_sync = true;
    }

    fn subtract_borders(
        &self,
        width: NonZeroU32,
        height: NonZeroU32,
    ) -> (Option<NonZeroU32>, Option<NonZeroU32>) {
        if self.state.contains(WindowState::FULLSCREEN) || self.render_data.is_none() {
            (Some(width), Some(height))
        } else {
            (
                NonZeroU32::new(width.get().saturating_sub(2 * BORDER_SIZE)),
                NonZeroU32::new(height.get().saturating_sub(HEADER_SIZE + 2 * BORDER_SIZE)),
            )
        }
    }

    fn add_borders(&self, width: u32, height: u32) -> (u32, u32) {
        if self.state.contains(WindowState::FULLSCREEN) || self.render_data.is_none() {
            (width, height)
        } else {
            (
                width + 2 * BORDER_SIZE,
                height + (HEADER_SIZE + 2 * BORDER_SIZE),
            )
        }
    }

    fn is_hidden(&self) -> bool {
        self.render_data.is_none()
    }

    fn location(&self) -> (i32, i32) {
        if self.state.contains(WindowState::FULLSCREEN) || self.is_hidden() {
            (0, 0)
        } else {
            self.render_data.as_ref().unwrap().parts[TOP_BORDER].pos
        }
    }

    fn is_dirty(&self) -> bool {
        self.dirty
    }

    fn draw(&mut self) -> bool {
        if self.render_data.is_none() {
            return false;
        }

        // Reset the dirty bit and sync option.
        self.dirty = false;
        let should_sync = mem::take(&mut self.should_sync);

        if self.state.contains(WindowState::FULLSCREEN) {
            // Don't draw the decorations for the full screen surface, and
            // keep the (bufferless) parts positioned inside the main
            // surface so the compositor does not enlarge the window's
            // extent beyond the screen.
            let parts = &self.render_data.as_ref().unwrap().parts;
            for part in parts {
                part.subsurface.set_position(0, 0);
                part.surface.attach(None, 0, 0);
                part.surface.commit();
            }
            return should_sync;
        }

        let is_active = self.state.contains(WindowState::ACTIVATED);
        let colors = themed_colors();
        let fill_color = premultiply(if is_active {
            colors.active_bg
        } else {
            colors.inactive_bg
        });

        // Make sure the title is shaped for the current state before drawing.
        self.reshape_title();

        let render_data = self.render_data.as_mut().unwrap();

        for (idx, part) in render_data.parts.iter().enumerate() {
            // We don't support fractional scaling here, so round up.
            let scale = self.scale_factor.ceil() as i32;

            let (buffer, mut canvas) = match self.pool.create_buffer(
                part.width as i32 * scale,
                part.height as i32 * scale,
                part.width as i32 * 4 * scale,
                wl_shm::Format::Argb8888,
            ) {
                Ok((buffer, canvas)) => (buffer, canvas),
                Err(_) => continue,
            };

            // Fill the canvas.
            for pixel in canvas.chunks_exact_mut(4) {
                pixel[0] = fill_color[0];
                pixel[1] = fill_color[1];
                pixel[2] = fill_color[2];
                pixel[3] = fill_color[3];
            }

            // Draw the title and buttons into the header.
            if idx == HEADER {
                if let Some(shaped) = self.shaped_title.as_ref() {
                    Self::draw_title(
                        &mut canvas,
                        part.width,
                        HEADER_SIZE,
                        scale as u32,
                        shaped,
                        self.buttons.iter().flatten().count(),
                    );
                }
                draw_buttons(
                    &self.buttons,
                    &mut canvas,
                    part.width,
                    HEADER_SIZE,
                    scale as u32,
                    self.state,
                    self.mouse_location,
                    self.pressed_button,
                    &colors,
                );
            }

            part.surface.set_buffer_scale(scale);
            if should_sync {
                part.subsurface.set_sync();
            } else {
                part.subsurface.set_desync();
            }

            // Update the subsurface position.
            part.subsurface.set_position(part.pos.0, part.pos.1);

            buffer
                .attach_to(&part.surface)
                .expect("failed to attach the buffer");
            if part.surface.version() >= 4 {
                part.surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
            } else {
                part.surface.damage(0, 0, i32::MAX, i32::MAX);
            }

            part.surface.commit();
        }

        should_sync
    }

    fn update_wm_capabilities(&mut self, capabilities: WindowManagerCapabilities) {
        self.dirty |= self.wm_capabilities != capabilities;
        self.wm_capabilities = capabilities;
        self.buttons = Self::supported_buttons(capabilities);
        if !self.resizable {
            self.buttons[1] = None;
        }
        self.pressed_button = None;
    }

    fn set_title(&mut self, title: impl Into<String>) {
        let title: String = title.into();
        if self.title != title {
            self.title = title;
            self.dirty = true;
            self.reshape_title();
        }
    }
}

/// Inner state to simplify dropping.
#[derive(Debug)]
struct FrameRenderData {
    /// The header subsurface.
    parts: [FramePart; 5],
}

impl FrameRenderData {
    fn new<State>(
        parent: &WlSurface,
        subcompositor: &SubcompositorState,
        queue_handle: &QueueHandle<State>,
    ) -> Self
    where
        State: Dispatch<WlSurface, SurfaceData> + Dispatch<WlSubsurface, SubsurfaceData> + 'static,
    {
        // The zero-sized decorated layout; the first resize() call fills in
        // the real dimensions through the same geometry function.
        let base = decorated_part_geometry(0, 0);
        let parts: [FramePart; 5] = std::array::from_fn(|index| {
            let (pos, width, height) = base[index];
            FramePart::new(
                subcompositor.create_subsurface(parent.clone(), queue_handle),
                width,
                height,
                pos,
            )
        });

        Self { parts }
    }
}

#[derive(Debug)]
struct FramePart {
    /// The surface used for the frame part.
    subsurface: WlSubsurface,

    /// The surface used for this part.
    surface: WlSurface,

    /// The width of the Frame part in logical pixels.
    width: u32,

    /// The height of the Frame part in logical pixels.
    height: u32,

    /// The position for the subsurface.
    pos: (i32, i32),
}

impl FramePart {
    fn new(surfaces: (WlSubsurface, WlSurface), width: u32, height: u32, pos: (i32, i32)) -> Self {
        let (subsurface, surface) = surfaces;
        // XXX sync subsurfaces with the main surface.
        subsurface.set_sync();
        Self {
            surface,
            subsurface,
            width,
            height,
            pos,
        }
    }
}

impl Drop for FramePart {
    fn drop(&mut self) {
        self.subsurface.destroy();
        self.surface.destroy();
    }
}

/// The location inside the
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum Location {
    /// The location doesn't belong to the frame.
    None,
    /// Header bar.
    Head,
    /// Top border.
    Top,
    /// Top right corner.
    TopRight,
    /// Right border.
    Right,
    /// Bottom right corner.
    BottomRight,
    /// Bottom border.
    Bottom,
    /// Bottom left corner.
    BottomLeft,
    /// Left border.
    Left,
    /// Top left corner.
    TopLeft,
    /// One of the buttons.
    Button(UIButton),
}

/// The frame button.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum UIButton {
    /// The minimize button, the left most.
    Minimize,
    /// The maximize button, in the middle.
    Maximize,
    /// The close botton, the right most.
    Close,
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUTTONS: [Option<UIButton>; 3] = [
        Some(UIButton::Close),
        Some(UIButton::Maximize),
        Some(UIButton::Minimize),
    ];

    #[test]
    fn decorated_geometry_covers_the_frame_exactly() {
        for (width, height) in [(1u32, 1u32), (320, 240), (1600, 1000), (3840, 2160)] {
            let geometry = decorated_part_geometry(width, height);
            // The tree extent is exactly the decorated frame: header and top
            // border above, side borders left/right, bottom border below.
            let min_x = geometry[TOP_BORDER].0 .0;
            let min_y = geometry[TOP_BORDER].0 .1;
            let max_x = geometry[RIGHT_BORDER].0 .0 + geometry[RIGHT_BORDER].1 as i32;
            let max_y = geometry[BOTTOM_BORDER].0 .1 + geometry[BOTTOM_BORDER].2 as i32;
            assert_eq!((min_x, min_y), (-(BORDER_SIZE as i32), -(HEADER_SIZE as i32 + BORDER_SIZE as i32)));
            assert_eq!(
                (max_x, max_y),
                (
                    width as i32 + BORDER_SIZE as i32,
                    height as i32 + BORDER_SIZE as i32
                )
            );
            // The header sits directly on the main surface.
            assert_eq!(geometry[HEADER].0 .1 + geometry[HEADER].2 as i32, 0);
            // The side borders span the header and the main surface.
            for part in [LEFT_BORDER, RIGHT_BORDER] {
                assert_eq!(geometry[part].2, height + HEADER_SIZE);
            }
        }
    }

    #[test]
    fn fullscreen_geometry_stays_inside_the_main_surface() {
        for (pos, width, height) in fullscreen_part_geometry() {
            assert_eq!((pos, width, height), ((0, 0), 1, 1));
        }
    }

    #[test]
    fn leaving_fullscreen_restores_the_decorated_geometry() {
        // Regression: the fullscreen collapse used to reset width/height/pos
        // while the restore path only rewrote some of them, leaving the
        // header 1px tall at the origin and the bottom border past the
        // surface. Restoring must reproduce the decorated geometry exactly.
        let (width, height) = (1280u32, 800u32);
        let decorated = decorated_part_geometry(width, height);
        let collapsed = fullscreen_part_geometry();
        for index in 0..5 {
            assert_ne!(decorated[index], collapsed[index]);
        }
        assert_eq!(decorated_part_geometry(width, height), decorated);
    }

    #[test]
    fn button_hit_regions_match_drawing_and_do_not_overlap() {
        for width in 1..400 {
            for (index, button) in BUTTONS.iter().flatten().enumerate() {
                if let Some(left) = button_left(width, index) {
                    assert_eq!(hit_button(&BUTTONS, left as f64, 18., width), Some(*button));
                    assert_eq!(
                        hit_button(&BUTTONS, (left + BUTTON_WIDTH - 1) as f64, 18., width),
                        Some(*button)
                    );
                    assert_eq!(hit_button(&BUTTONS, left as f64, -1., width), None);
                    assert_eq!(
                        hit_button(&BUTTONS, left as f64, HEADER_SIZE as f64, width),
                        None
                    );
                }
            }
            assert_eq!(hit_button(&BUTTONS, width as f64, 18., width), None);
        }
    }

    #[test]
    fn missing_capabilities_compact_the_buttons() {
        let buttons = [Some(UIButton::Close), None, Some(UIButton::Minimize)];
        let left = button_left(300, 1).unwrap() as f64;
        assert_eq!(
            hit_button(&buttons, left, 18., 300),
            Some(UIButton::Minimize)
        );
    }

    #[test]
    fn release_requires_a_matching_press_and_consumes_it() {
        let mut pressed = None;
        assert_eq!(
            released_button(&mut pressed, Location::Button(UIButton::Close)),
            None
        );
        pressed = Some(UIButton::Minimize);
        assert_eq!(
            released_button(&mut pressed, Location::Button(UIButton::Close)),
            None
        );
        assert_eq!(pressed, None);
        pressed = Some(UIButton::Close);
        assert_eq!(released_button(&mut pressed, Location::Head), None);
        pressed = Some(UIButton::Close);
        assert_eq!(
            released_button(&mut pressed, Location::Button(UIButton::Close)),
            Some(UIButton::Close)
        );
        assert_eq!(
            released_button(&mut pressed, Location::Button(UIButton::Close)),
            None
        );
    }

    #[test]
    fn button_raster_handles_tiny_surfaces_and_changes_restore_icon() {
        let colors = themed_colors();
        for scale in [1, 2, 3] {
            for width in [1, 24, 40, 80, 300] {
                let mut canvas = vec![0; (width * HEADER_SIZE * scale * scale * 4) as usize];
                draw_buttons(
                    &BUTTONS,
                    &mut canvas,
                    width,
                    HEADER_SIZE,
                    scale,
                    WindowState::ACTIVATED,
                    Location::None,
                    None,
                    &colors,
                );
                let normal = canvas.clone();
                canvas.fill(0);
                draw_buttons(
                    &BUTTONS,
                    &mut canvas,
                    width,
                    HEADER_SIZE,
                    scale,
                    WindowState::ACTIVATED | WindowState::MAXIMIZED,
                    Location::None,
                    None,
                    &colors,
                );
                if width >= 80 {
                    assert_ne!(normal, canvas);
                }
            }
        }
    }
}
