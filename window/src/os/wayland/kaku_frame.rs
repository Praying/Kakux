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
const HEADER_SIZE: u32 = 30;

/// The size of the border.
const BORDER_SIZE: u32 = 6;

const HEADER: usize = 0;
const TOP_BORDER: usize = 1;
const RIGHT_BORDER: usize = 2;
const BOTTOM_BORDER: usize = 3;
const LEFT_BORDER: usize = 4;

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
        for (idx, &button) in buttons.iter().flatten().enumerate() {
            let idx = idx as u32;
            if w >= (idx + 1) * HEADER_SIZE
                && x >= f64::from(w - (idx + 1) * HEADER_SIZE)
                && x <= f64::from(w - idx * HEADER_SIZE)
                && y <= f64::from(HEADER_SIZE)
                && y >= f64::from(0)
            {
                return Location::Button(button);
            }
        }

        Location::Head
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
        let target_px = (HEADER_SIZE as f32 * scale as f32 * 0.6 * size_factor).min(max_cell_px);
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
    fn draw_title(canvas: &mut [u8], width: u32, height: u32, scale: u32, shaped: &ShapedTitle) {
        let Some(mut pixmap) = PixmapMut::from_bytes(canvas, width * scale, height * scale) else {
            return;
        };
        let scale_f = scale as f32;
        // The buttons occupy the rightmost button-count header cells; keep a
        // symmetric 8px margin on both sides of the usable title region.
        let left_margin = 8. * scale_f;
        let right_reserve = 3. * HEADER_SIZE as f32 * scale_f + 8. * scale_f;
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
            let gx = x + item.x_offset + item.bearing_x;
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

    /// Draw themed buttons into the header canvas: hover-highlighted
    /// squares with the separator line below the header.
    fn draw_buttons(
        buttons: &[Option<UIButton>],
        canvas: &mut [u8],
        width: u32,
        height: u32,
        scale: u32,
        is_active: bool,
        mouse_location: &Location,
        colors: &ThemedColors,
    ) {
        let scale = scale as usize;
        // The separator line between header and window contents.
        let sep = premultiply(if is_active {
            colors.active_separator
        } else {
            colors.inactive_separator
        });
        let line_y = height as usize * scale - scale;
        for pixel in canvas
            [line_y * width as usize * scale * 4..(line_y + 1) * width as usize * scale * 4]
            .chunks_exact_mut(4)
        {
            pixel[0] = sep[0];
            pixel[1] = sep[1];
            pixel[2] = sep[2];
            pixel[3] = sep[3];
        }

        for (idx, &button) in buttons.iter().flatten().enumerate() {
            if width < (idx + 1) as u32 * HEADER_SIZE {
                continue;
            }
            let hovered = is_active
                && matches!((mouse_location, button), (Location::Button(b), _) if *b == button);
            let (bg, fg) = if hovered {
                (colors.button_hover_bg, colors.button_hover_fg)
            } else if is_active {
                (colors.active_bg, colors.active_fg)
            } else {
                (colors.inactive_bg, colors.inactive_fg)
            };

            let x_start = width as usize * scale - (idx + 1) * HEADER_SIZE as usize * scale;
            let h = HEADER_SIZE as usize * scale;

            let bg = premultiply(bg);
            for y in 0..h {
                let row = &mut canvas[(x_start + y * width as usize * scale) * 4
                    ..(x_start + y * width as usize * scale + h) * 4];
                for pixel in row.chunks_exact_mut(4) {
                    pixel[0] = bg[0];
                    pixel[1] = bg[1];
                    pixel[2] = bg[2];
                    pixel[3] = bg[3];
                }
            }

            let fg = premultiply(fg);
            let cx = x_start + h / 2;
            let cy = h / 2;
            let stroke = (scale as f32 * 1.5).ceil() as usize;
            match button {
                UIButton::Close => {
                    // An X
                    for d in -((h / 4) as i32)..=(h / 4) as i32 {
                        for s in -(stroke as i32 / 2)..=(stroke as i32 / 2) {
                            for (dx, dy) in [(d + s, d), (d + s, -d)] {
                                let x = cx as i32 + dx;
                                let y = cy as i32 + dy;
                                if x >= 0
                                    && y >= 0
                                    && (x as usize) < width as usize * scale
                                    && (y as usize) < h
                                {
                                    let idx =
                                        (y as usize * width as usize * scale + x as usize) * 4;
                                    canvas[idx] = fg[0];
                                    canvas[idx + 1] = fg[1];
                                    canvas[idx + 2] = fg[2];
                                    canvas[idx + 3] = fg[3];
                                }
                            }
                        }
                    }
                }
                UIButton::Maximize => {
                    // A square outline
                    let r = h / 5;
                    for y in cy - r..cy + r {
                        for x in cx - r..cx + r {
                            let edge = (y - (cy - r)) < stroke
                                || ((cy + r) - 1 - y) < stroke
                                || (x - (cx - r)) < stroke
                                || ((cx + r) - 1 - x) < stroke;
                            if edge {
                                let idx = (y * width as usize * scale + x) * 4;
                                canvas[idx] = fg[0];
                                canvas[idx + 1] = fg[1];
                                canvas[idx + 2] = fg[2];
                                canvas[idx + 3] = fg[3];
                            }
                        }
                    }
                }
                UIButton::Minimize => {
                    // An underline
                    let underline_y = cy + h / 4;
                    for y in underline_y..underline_y + stroke {
                        for x in cx - h / 4..cx + h / 4 {
                            let idx = (y * width as usize * scale + x) * 4;
                            canvas[idx] = fg[0];
                            canvas[idx + 1] = fg[1];
                            canvas[idx + 2] = fg[2];
                            canvas[idx + 3] = fg[3];
                        }
                    }
                }
            }
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

        let resize = pressed && self.resizable;
        match self.mouse_location {
            Location::Head if pressed => Some(FrameAction::Move),
            Location::Button(UIButton::Close) if !pressed => Some(FrameAction::Close),
            Location::Button(UIButton::Minimize) if !pressed => Some(FrameAction::Minimize),
            Location::Button(UIButton::Maximize)
                if !pressed && !self.state.contains(WindowState::MAXIMIZED) =>
            {
                Some(FrameAction::Maximize)
            }
            Location::Button(UIButton::Maximize)
                if !pressed && self.state.contains(WindowState::MAXIMIZED) =>
            {
                Some(FrameAction::UnMaximize)
            }
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

        if self.state.contains(WindowState::FULLSCREEN) {
            // Nothing is drawn in fullscreen, but the compositor derives the
            // window's extent from the whole surface tree: frame parts left
            // at their decorated positions would stick out beyond the
            // screen and make it scale the window down to fit, cutting the
            // bottom rows. Collapse every part inside the main surface.
            for part in parts.iter_mut() {
                part.pos = (0, 0);
                part.width = 1;
                part.height = 1;
            }
            self.dirty = true;
            self.should_sync = true;
            return;
        }

        parts[HEADER].width = width;

        parts[TOP_BORDER].width = width + 2 * BORDER_SIZE;

        parts[BOTTOM_BORDER].width = width + 2 * BORDER_SIZE;
        parts[BOTTOM_BORDER].pos.1 = height as i32;

        parts[LEFT_BORDER].height = height + HEADER_SIZE;

        parts[RIGHT_BORDER].height = parts[LEFT_BORDER].height;
        parts[RIGHT_BORDER].pos.0 = width as i32;

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
                    Self::draw_title(&mut canvas, part.width, HEADER_SIZE, scale as u32, shaped);
                }
                Self::draw_buttons(
                    &self.buttons,
                    &mut canvas,
                    part.width,
                    HEADER_SIZE,
                    scale as u32,
                    is_active,
                    &self.mouse_location,
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
        let parts = [
            // Header.
            FramePart::new(
                subcompositor.create_subsurface(parent.clone(), queue_handle),
                0,
                HEADER_SIZE,
                (0, -(HEADER_SIZE as i32)),
            ),
            // Top border.
            FramePart::new(
                subcompositor.create_subsurface(parent.clone(), queue_handle),
                0,
                BORDER_SIZE,
                (
                    -(BORDER_SIZE as i32),
                    -(HEADER_SIZE as i32 + BORDER_SIZE as i32),
                ),
            ),
            // Right border.
            FramePart::new(
                subcompositor.create_subsurface(parent.clone(), queue_handle),
                BORDER_SIZE,
                0,
                (0, -(HEADER_SIZE as i32)),
            ),
            // Bottom border.
            FramePart::new(
                subcompositor.create_subsurface(parent.clone(), queue_handle),
                0,
                BORDER_SIZE,
                (-(BORDER_SIZE as i32), 0),
            ),
            // Left border.
            FramePart::new(
                subcompositor.create_subsurface(parent.clone(), queue_handle),
                BORDER_SIZE,
                0,
                (-(BORDER_SIZE as i32), -(HEADER_SIZE as i32)),
            ),
        ];

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
