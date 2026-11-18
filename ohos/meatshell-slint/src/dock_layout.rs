//! Dock geometry for the OHOS layout: welcome sidebar (left) + resource
//! sidebar (right) around the central terminal area.
//!
//! The upstream desktop app computes these rects in `src/app/dock_stacks.rs`
//! and `src/app/panes.rs`. The OHOS port only ever shows two fixed edge panels,
//! so the same properties are derived directly here: `dock-panels` carries the
//! panel rects, `dock-central-*` is what is left for `rest`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use slint::{Model, ModelRc, VecModel};

use crate::{AppWindow, DividerGeomInfo, PanelGeomInfo};

/// Sidebar widths in logical px (matches the upstream defaults).
pub const WELCOME_WIDTH: f32 = 240.0;
pub const SIDEBAR_WIDTH: f32 = 280.0;
const WELCOME_MIN: f32 = 160.0;
const WELCOME_MAX: f32 = 480.0;
const SIDEBAR_MIN: f32 = 180.0;
const SIDEBAR_MAX: f32 = 420.0;
/// Keep the terminal usable no matter how wide the side panels get.
const MIN_CENTRAL: f32 = 420.0;

/// Width of the edge icon strip a collapsed panel leaves behind (upstream
/// `welcome-strip`). The strip itself is drawn by app.slint outside the dock
/// panels, so the central rect must stop short of it or it would be covered.
const COLLAPSED_STRIP: f32 = 36.0;

thread_local! {
    /// Last dock-area size, so splitter/collapse callbacks can re-apply the
    /// layout without waiting for another `dock-area-resized`.
    static LAST_SIZE: Cell<(f32, f32)> = const { Cell::new((1366.0, 730.0)) };
    /// The panel model handed to the UI. Rows are updated in place while
    /// dragging so the splitter keeps its grab.
    static PANELS: RefCell<Option<Rc<VecModel<PanelGeomInfo>>>> = const { RefCell::new(None) };
}

pub fn last_size() -> (f32, f32) {
    LAST_SIZE.with(|c| c.get())
}

/// Restore the two edge panels to their defaults. Called once at startup.
pub fn init(ui: &AppWindow) {
    ui.set_welcome_as_sidebar(true);
    ui.set_welcome_sidebar_dock("left".into());
    ui.set_welcome_collapsed(false);
    ui.set_welcome_sidebar_width(WELCOME_WIDTH);
    ui.set_sidebar_dock("right".into());
    ui.set_sidebar_collapsed(false);
    ui.set_sidebar_width(SIDEBAR_WIDTH);
    let model = Rc::new(VecModel::<PanelGeomInfo>::default());
    ui.set_dock_panels(ModelRc::from(model.clone()));
    ui.set_dock_dividers(ModelRc::from(Rc::new(VecModel::<DividerGeomInfo>::default())));
    PANELS.with(|slot| *slot.borrow_mut() = Some(model));
}

fn set_panels(ui: &AppWindow, panels: Vec<PanelGeomInfo>) {
    PANELS.with(|slot| {
        let mut slot = slot.borrow_mut();
        let model = slot.get_or_insert_with(|| {
            let model = Rc::new(VecModel::<PanelGeomInfo>::default());
            ui.set_dock_panels(ModelRc::from(model.clone()));
            model
        });
        if model.row_count() != panels.len() {
            model.set_vec(panels);
        } else {
            for (i, panel) in panels.into_iter().enumerate() {
                model.set_row_data(i, panel);
            }
        }
    });
}

/// Recompute the three-column layout for a dock area of `w` × `h`.
pub fn apply_dock(ui: &AppWindow, w: f32, h: f32) {
    let w = w.max(1.0);
    let h = h.max(1.0);
    LAST_SIZE.with(|c| c.set((w, h)));

    let welcome_collapsed = ui.get_welcome_collapsed();
    let sidebar_collapsed = ui.get_sidebar_collapsed();
    let mut left = if welcome_collapsed {
        COLLAPSED_STRIP
    } else {
        ui.get_welcome_sidebar_width()
            .clamp(WELCOME_MIN, WELCOME_MAX)
    };
    let mut right = if sidebar_collapsed {
        COLLAPSED_STRIP
    } else {
        ui.get_sidebar_width().clamp(SIDEBAR_MIN, SIDEBAR_MAX)
    };
    left = left.min(w * 0.3);
    right = right.min(w * 0.35);

    // Squeeze the resource panel first, then the welcome panel.
    let min_central = MIN_CENTRAL.min(w * 0.6);
    if w - left - right < min_central {
        let cut = (min_central - (w - left - right)).min(right);
        right -= cut;
    }
    if w - left - right < min_central {
        left = (left - (min_central - (w - left - right))).max(0.0);
    }

    ui.set_dock_central_x(left);
    ui.set_dock_central_y(0.0);
    ui.set_dock_central_w((w - left - right).max(0.0));
    ui.set_dock_central_h(h);

    let mut panels = Vec::new();
    if !welcome_collapsed {
        panels.push(PanelGeomInfo {
            kind: "welcome".into(),
            edge: "left".into(),
            x: 0.0,
            y: 0.0,
            w: left,
            h,
        });
    }
    if !sidebar_collapsed {
        panels.push(PanelGeomInfo {
            kind: "sidebar".into(),
            edge: "right".into(),
            x: w - right,
            y: 0.0,
            w: right,
            h,
        });
    }
    set_panels(ui, panels);
}

/// Splitter drag reported by `panel-extent-drag(index, pos)`: `pos` is the new
/// extent of panel `index` in dock-area coordinates.
pub fn drag_panel(ui: &AppWindow, index: i32, pos: f32) {
    let kind = PANELS.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|model| model.row_data(index.max(0) as usize))
            .map(|panel| panel.kind.to_string())
    });
    match kind.as_deref() {
        Some("welcome") => ui.set_welcome_sidebar_width(pos.clamp(WELCOME_MIN, WELCOME_MAX)),
        Some("sidebar") => ui.set_sidebar_width(pos.clamp(SIDEBAR_MIN, SIDEBAR_MAX)),
        _ => return,
    }
    let (w, h) = last_size();
    apply_dock(ui, w, h);
}
