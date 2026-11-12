use super::corners::*;
use crate::tabbar::TabBarItem;
use crate::termwindow::box_model::*;
use crate::termwindow::{TermWindow, UIItemType};
use crate::utilsprites::RenderMetrics;
use config::{Dimension, DimensionContext, TabBarColors};
use wezterm_term::color::ColorPalette;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabControl {
    ToggleLayout,
    Previous,
    Next,
}

pub(crate) fn toggle_layout_overrides(
    value: &wezterm_dynamic::Value,
    vertical: bool,
) -> wezterm_dynamic::Value {
    use wezterm_dynamic::Value;
    let mut overrides = match value.clone() {
        Value::Object(object) => object,
        _ => Default::default(),
    };
    for (key, value) in [
        ("vertical_tab_bar", !vertical),
        ("use_fancy_tab_bar", true),
        ("tab_bar_at_bottom", false),
    ] {
        overrides.insert(Value::String(key.into()), Value::Bool(value));
    }
    Value::Object(overrides)
}

pub(crate) fn sidebar_width(enabled: bool, width: usize, dpi: usize) -> f32 {
    if enabled {
        (224. * dpi as f32 / 96.).min(width as f32 * 0.35).floor()
    } else {
        0.
    }
}

fn visible_tabs(count: usize, active: usize, capacity: usize) -> std::ops::Range<usize> {
    let capacity = capacity.max(1).min(count);
    let start = active.saturating_sub(capacity / 2).min(count - capacity);
    start..start + capacity
}

fn rounded(radius: f32) -> Corners {
    let corner = |poly| SizedPoly {
        width: Dimension::Pixels(radius),
        height: Dimension::Pixels(radius),
        poly,
    };
    Corners {
        top_left: corner(TOP_LEFT_ROUNDED_CORNER),
        top_right: corner(TOP_RIGHT_ROUNDED_CORNER),
        bottom_left: corner(BOTTOM_LEFT_ROUNDED_CORNER),
        bottom_right: corner(BOTTOM_RIGHT_ROUNDED_CORNER),
    }
}

impl TermWindow {
    pub fn workspace_tabs_enabled(&self) -> bool {
        self.config.vertical_tab_bar
            || (!cfg!(target_os = "macos") && self.config.use_fancy_tab_bar)
    }

    pub fn tab_sidebar_width(&self) -> f32 {
        sidebar_width(
            self.config.vertical_tab_bar && self.show_tab_bar,
            self.dimensions.pixel_width,
            self.dimensions.dpi,
        )
    }

    pub fn build_workspace_tabs(&self, palette: &ColorPalette) -> anyhow::Result<ComputedElement> {
        let vertical = self.config.vertical_tab_bar;
        let scale = self.dimensions.dpi as f32 / 96.;
        let row = 40. * scale;
        let gap = 4. * scale;
        let border = self.get_os_border();
        let width = if vertical {
            self.tab_sidebar_width()
        } else {
            self.dimensions.pixel_width as f32
        };
        let height = if vertical {
            (self.dimensions.pixel_height as f32 - (border.top + border.bottom).get() as f32)
                .max(0.)
        } else {
            self.tab_bar_pixel_height()?
        };
        let origin_y = if vertical {
            border.top.get() as f32
        } else {
            self.tab_bar_y(height)
        };
        let font = self.fonts.title_font()?;
        let metrics = RenderMetrics::with_font_metrics(&font.metrics());
        let colors = self
            .config
            .resolved_palette
            .tab_bar
            .clone()
            .unwrap_or_else(TabBarColors::default);
        let foreground = palette.foreground.to_linear();
        let opaque = super::forces_opaque_kaku_tui_window_background(&self.get_panes_to_render());
        let background = palette.background.to_linear().mul_alpha(if opaque {
            1.
        } else {
            self.config.window_background_opacity
        });
        let base = ElementColors {
            border: BorderColor::new(background),
            bg: background.into(),
            text: foreground.into(),
        };
        let hover = ElementColors {
            border: BorderColor::new(colors.inactive_tab_hover().bg_color.to_linear()),
            bg: colors.inactive_tab_hover().bg_color.to_linear().into(),
            ..base.clone()
        };
        let context = LayoutContext {
            width: DimensionContext {
                dpi: self.dimensions.dpi as f32,
                pixel_max: width,
                pixel_cell: metrics.cell_size.width as f32,
            },
            height: DimensionContext {
                dpi: self.dimensions.dpi as f32,
                pixel_max: height,
                pixel_cell: metrics.cell_size.height as f32,
            },
            bounds: euclid::rect(0., 0., width, height),
            metrics: &metrics,
            gl_state: self.render_state.as_ref().unwrap(),
            zindex: 10,
        };
        let mut root = self.compute_element(
            &context,
            &Element::new(&font, ElementContent::Text(String::new())).colors(base.clone()),
        )?;
        root.bounds = context.bounds;
        root.border_rect = root.bounds;
        root.padding = root.bounds;
        root.content_rect = root.bounds;
        root.item_type = Some(UIItemType::TabBar(TabBarItem::None));
        root.colors.border = BorderColor::new(colors.inactive_tab_edge().to_linear());
        if vertical {
            root.border.right = scale;
            root.padding.size.width = (width - scale).max(0.);
        } else {
            root.border.bottom = scale;
            root.padding.size.height = (height - scale).max(0.);
        }
        let mut children = Vec::new();
        let mut place = |element: Element,
                         x: f32,
                         y: f32,
                         item_width: f32,
                         item_height: f32|
         -> anyhow::Result<()> {
            let bounds = euclid::rect(x, y, item_width.max(0.), item_height.max(0.));
            if bounds.max_x() > width + 0.5
                || bounds.max_y() > height + 0.5
                || item_width < 1.
                || item_height < 1.
            {
                return Ok(());
            }
            let local = LayoutContext { bounds, ..context };
            let content_height = match &element.content {
                ElementContent::Poly { poly, .. } => poly.height.evaluate_as_pixels(context.height),
                _ => metrics.cell_size.height as f32,
            };
            let element = element
                .margin(BoxDimension::default())
                .float(Float::None)
                .max_width(Some(Dimension::Pixels((item_width - 2. * gap).max(0.))))
                .padding(BoxDimension {
                    left: Dimension::Pixels(gap),
                    right: Dimension::Pixels(gap),
                    top: Dimension::Pixels(((item_height - content_height) / 2.).max(0.)),
                    bottom: Dimension::Pixels(0.),
                })
                .border_corners(Some(rounded(6. * scale)));
            let mut computed = self.compute_element(&local, &element)?;
            computed.bounds = bounds;
            computed.border_rect = computed.bounds;
            computed.padding = computed.bounds;
            children.push(computed);
            Ok(())
        };
        let control = |label: &str, action| {
            Element::new(&font, ElementContent::Text(label.into()))
                .item_type(UIItemType::TabBarControl(action))
                .colors(base.clone())
                .hover_colors(Some(hover.clone()))
        };
        let toggle_width = (88. * scale).min((width - 40. * scale).max(0.));
        place(
            control(
                if vertical { "Top tabs" } else { "Sidebar" },
                TabControl::ToggleLayout,
            ),
            gap,
            gap,
            (toggle_width - gap * 2.).max(0.),
            row - 2. * gap,
        )?;
        let new_width = 32. * scale;
        if self.config.show_new_tab_button_in_tab_bar {
            let plus = Element::new(
                &font,
                ElementContent::Poly {
                    poly: SizedPoly {
                        poly: super::fancy_tab_bar::PLUS_BUTTON,
                        width: Dimension::Pixels(12. * scale),
                        height: Dimension::Pixels(12. * scale),
                    },
                    line_width: 2,
                },
            )
            .item_type(UIItemType::TabBar(TabBarItem::NewTabButton))
            .colors(base.clone())
            .hover_colors(Some(hover.clone()));
            place(
                plus,
                (width - new_width - gap).max(0.),
                gap,
                new_width,
                row - gap * 2.,
            )?;
        }
        let tabs: Vec<_> = self
            .tab_bar
            .items()
            .iter()
            .filter(|entry| matches!(entry.item, TabBarItem::Tab { .. }))
            .collect();
        let statuses: Vec<_> = self
            .tab_bar
            .items()
            .iter()
            .filter(|entry| {
                matches!(entry.item, TabBarItem::LeftStatus | TabBarItem::RightStatus)
                    && !entry.title.as_str().trim().is_empty()
            })
            .collect();
        let status_width = if vertical {
            width - gap * 2.
        } else {
            (width * 0.12).min(160. * scale)
        };
        let status_space = statuses.len() as f32 * if vertical { row } else { status_width };
        let active = tabs
            .iter()
            .position(|entry| matches!(entry.item, TabBarItem::Tab { active: true, .. }))
            .unwrap_or(0);
        let available = if vertical {
            (height - row * 2. - status_space).max(0.)
        } else {
            (width - toggle_width - new_width - row * 2. - gap * 2. - status_space).max(0.)
        };
        let capacity =
            (available / if vertical { row + gap } else { 100. * scale }).floor() as usize;
        let visible = visible_tabs(tabs.len(), active, capacity);
        let tab_width = if vertical {
            width - gap * 2.
        } else {
            (available / visible.len().max(1) as f32).min(220. * scale) - gap
        };
        for (slot, index) in visible.clone().enumerate() {
            let entry = tabs[index];
            let TabBarItem::Tab { tab_idx, active } = entry.item else {
                continue;
            };
            let style = if active {
                colors.active_tab()
            } else {
                colors.inactive_tab()
            };
            let tab_colors = ElementColors {
                border: BorderColor::new(style.bg_color.to_linear()),
                bg: style.bg_color.to_linear().into(),
                text: style.fg_color.to_linear().into(),
                ..base.clone()
            };
            let x = if vertical {
                gap
            } else {
                toggle_width + slot as f32 * (tab_width + gap)
            };
            let y = if vertical {
                row + slot as f32 * (row + gap)
            } else {
                gap
            };
            let item_height = row - gap * 2.;
            let mut tab = Element::with_line(&font, &entry.title, palette)
                .item_type(UIItemType::TabBar(entry.item))
                .colors(tab_colors.clone())
                .hover_colors(if active { None } else { Some(hover.clone()) });
            if let ElementContent::Children(ref mut pieces) = tab.content {
                for piece in pieces {
                    piece.max_width = Some(Dimension::Pixels((tab_width - 36. * scale).max(0.)));
                }
            }
            place(tab, x, y, tab_width, item_height)?;
            if self.config.show_close_tab_button_in_tabs {
                let close =
                    super::fancy_tab_bar::make_x_button(&font, &metrics, &colors, tab_idx, active)
                        .colors(tab_colors)
                        .hover_colors(Some(hover.clone()));
                place(
                    close,
                    x + tab_width - 26. * scale,
                    y,
                    22. * scale,
                    item_height,
                )?;
            }
        }
        if visible.len() < tabs.len() {
            let y = if vertical {
                (height - row - status_space).max(0.)
            } else {
                gap
            };
            let x = if vertical {
                gap
            } else {
                (width - new_width - row * 2. - gap - status_space).max(0.)
            };
            place(
                control("‹", TabControl::Previous),
                x,
                y,
                row - gap,
                row - gap * 2.,
            )?;
            place(
                control("›", TabControl::Next),
                x + row,
                y,
                row - gap,
                row - gap * 2.,
            )?;
        }
        for (index, entry) in statuses.iter().enumerate() {
            let status = Element::with_line(&font, &entry.title, palette).colors(base.clone());
            let x = if vertical {
                gap
            } else {
                width - new_width - gap - status_space + index as f32 * status_width
            };
            let y = if vertical {
                height - status_space + index as f32 * row
            } else {
                gap
            };
            place(status, x, y, status_width, row - gap * 2.)?;
        }
        root.content = ComputedElementContent::Children(children);
        root.translate(euclid::vec2(0., origin_y));
        Ok(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_toggle_preserves_unrelated_window_overrides() {
        use wezterm_dynamic::Value;
        let mut original = wezterm_dynamic::Object::default();
        original.insert(Value::String("font_size".into()), Value::U64(18));
        let vertical = toggle_layout_overrides(&Value::Object(original.clone()), false);
        let Value::Object(horizontal) = toggle_layout_overrides(&vertical, true) else {
            panic!("expected overrides");
        };
        assert_eq!(
            horizontal.get(&Value::String("font_size".into())),
            Some(&Value::U64(18))
        );
        assert_eq!(
            horizontal.get(&Value::String("vertical_tab_bar".into())),
            Some(&Value::Bool(false))
        );
        assert_eq!(
            horizontal.get(&Value::String("use_fancy_tab_bar".into())),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            horizontal.get(&Value::String("tab_bar_at_bottom".into())),
            Some(&Value::Bool(false))
        );
        assert_eq!(original.len(), 1);
        assert!(matches!(
            toggle_layout_overrides(&Value::Null, false),
            Value::Object(_)
        ));
    }

    #[test]
    fn sidebar_reserves_space_without_starving_terminal() {
        assert_eq!(sidebar_width(false, 1200, 96), 0.);
        assert_eq!(sidebar_width(true, 1200, 96), 224.);
        assert_eq!(sidebar_width(true, 2400, 192), 448.);
        assert_eq!(sidebar_width(true, 400, 96), 140.);
    }

    #[test]
    fn overflow_always_keeps_active_tab_visible() {
        for count in 1..100 {
            for active in 0..count {
                for capacity in 0..20 {
                    let visible = visible_tabs(count, active, capacity);
                    assert!(visible.contains(&active));
                    assert!(visible.end <= count);
                    assert!(visible.len() <= capacity.max(1));
                }
            }
        }
        assert_eq!(visible_tabs(0, 0, 0), 0..0);
    }
}
