//! The main edit view.

use std::any::Any;
use std::cmp::min;
use std::fmt;
use std::mem;
use std::ops::Range;
use std::sync::{Mutex, Weak};

use serde_json::Value;

use winapi::um::winuser::{
    VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_NEXT, VK_OEM_4, VK_OEM_6,
    VK_PRIOR, VK_RETURN, VK_RIGHT, VK_TAB, VK_UP,
};

use direct2d::brush::SolidColorBrush;
use direct2d::math::RectF;
use direct2d::RenderTarget;
use directwrite::TextFormat;

use druid_win_shell::window::{MouseButton, M_ALT, M_CTRL, M_SHIFT};

use druid::widget::Widget;
use druid::Ui;

use druid::KeyVariant;
use druid::{BoxConstraints, Geometry, LayoutResult};
use druid::{HandlerCtx, Id, KeyEvent, LayoutCtx, MouseEvent, PaintCtx};

use crate::linecache::LineCache;
use crate::rpc::Core;
use crate::textline::TextLine;

/// The commands the [`EditView`] widget accepts through `poke`.
#[derive(Debug)]
pub enum EditViewCommands {
    ViewId(String),
    ApplyUpdate(Value),
    ScrollTo(usize),
    Core(Weak<Mutex<Core>>),
    Undo,
    Redo,
    UpperCase,
    LowerCase,
    Transpose,
    AddCursorAbove,
    AddCursorBelow,
    SingleSelection,
    SelectAll,
}

type Method = String;
type Params = Value;

/// State and behavior for one editor view.
pub struct EditView {
    view_id: Option<String>,
    line_cache: LineCache,
    dwrite_factory: directwrite::Factory,
    resources: Option<Resources>,
    scroll_offset: f32,
    size: (f32, f32), // in px units
    viewport: Range<usize>,
    core: Weak<Mutex<Core>>,
    pending: Vec<(Method, Params)>,
}

impl fmt::Debug for EditView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EditView")
            .field("view_id", &self.view_id)
            .field("line_cache", &self.line_cache)
            .field("dwrite_factory", &"...")
            .field("resources", &self.resources)
            .field("scroll_offset", &self.scroll_offset)
            .field("size", &self.size)
            .field("viewport", &self.viewport)
            .field("core", &self.core)
            .field("pending", &self.pending)
            .finish()
    }
}

struct Resources {
    fg: SolidColorBrush,
    bg: SolidColorBrush,
    sel: SolidColorBrush,
    text_format: TextFormat,
}

impl fmt::Debug for Resources {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Resources {{ ... }}")
    }
}

const TOP_PAD: f32 = 6.0;
const LEFT_PAD: f32 = 6.0;
const LINE_SPACE: f32 = 17.0;

impl Widget for EditView {
    fn paint(&mut self, paint_ctx: &mut PaintCtx, geom: &Geometry) {
        // TODO(Olive): Cache resources, and flush cache when the render target is re-created.
        self.size = geom.size;
        let resources = self.create_resources(paint_ctx);
        let rt = paint_ctx.render_target();
        let rect = RectF::from((0.0, 0.0, self.size.0, self.size.1));
        rt.fill_rectangle(rect, &resources.bg);

        let first_line = self.y_to_line(0.0);
        let last_line = min(self.y_to_line(self.size.1) + 1, self.line_cache.height());

        let x0 = LEFT_PAD;
        let mut y = line_to_content_y(first_line) - self.scroll_offset;
        for line_num in first_line..last_line {
            if let Some(textline) = self.get_text_line(line_num) {
                textline.draw_bg(rt, x0, y, &resources.sel);
            }
            y += LINE_SPACE;
        }
        let mut y = line_to_content_y(first_line) - self.scroll_offset;
        for line_num in first_line..last_line {
            if let Some(textline) = self.get_text_line(line_num) {
                textline.draw_text(rt, x0, y, &resources.fg);
                textline.draw_cursor(rt, x0, y, &resources.fg);
            }
            y += LINE_SPACE;
        }
        self.resources = Some(resources);
    }

    fn layout(
        &mut self,
        bc: &BoxConstraints,
        _children: &[Id],
        _size: Option<(f32, f32)>,
        _ctx: &mut LayoutCtx,
    ) -> LayoutResult {
        let size = bc.constrain((0.0, 0.0));
        self.size = size;
        self.update_viewport();
        LayoutResult::Size(size)
    }

    fn mouse(&mut self, event: &MouseEvent, _ctx: &mut HandlerCtx) -> bool {
        let MouseEvent {
            x,
            y,
            mods: _,
            which,
            count,
        } = *event;
        if which == MouseButton::Left && count == 1 {
            let (line, col) = self.xy_to_line_col(x, y);
            let params = json!({
                "ty": "point_select",
                "line": line,
                "col": col,
            });
            self.send_edit_cmd("gesture", &params);
        }
        false
    }

    fn poke(&mut self, payload: &mut dyn Any, ctx: &mut HandlerCtx) -> bool {
        if let Some(cmd) = payload.downcast_ref::<EditViewCommands>() {
            match cmd {
                EditViewCommands::ViewId(view_id) => {
                    self.view_id = Some(view_id.to_string());
                    self.viewport = 0..0; // zorch viewport
                    self.update_viewport();

                    // Fire off the pending notifications
                    let pending = mem::take(&mut self.pending);
                    for notification in pending {
                        let (method, params) = notification;
                        self.send_edit_cmd(&method, &params);
                    }
                }
                EditViewCommands::ApplyUpdate(update) => {
                    self.apply_update(update);
                    ctx.invalidate();
                }
                EditViewCommands::ScrollTo(line) => {
                    self.scroll_to(*line);
                    ctx.invalidate();
                }
                EditViewCommands::Core(core) => {
                    self.core = core.clone();
                }
                EditViewCommands::Undo => {
                    self.send_action("undo");
                }
                EditViewCommands::Redo => {
                    self.send_action("redo");
                }
                EditViewCommands::UpperCase => {
                    self.send_action("uppercase");
                }
                EditViewCommands::LowerCase => {
                    self.send_action("lowercase");
                }
                EditViewCommands::Transpose => {
                    self.send_action("transpose");
                }
                EditViewCommands::AddCursorAbove => {
                    // Note: some subtlety around find, the escape key cancels it, but the menu
                    // shouldn't.
                    self.send_action("add_selection_above");
                }
                EditViewCommands::AddCursorBelow => {
                    // Note: some subtlety around find, the escape key cancels it, but the menu
                    // shouldn't.
                    self.send_action("add_selection_below");
                }
                EditViewCommands::SingleSelection => {
                    // Note: some subtlety around find, the escape key cancels it, but the menu
                    // shouldn't.
                    self.send_action("cancel_operation");
                }
                EditViewCommands::SelectAll => {
                    // Note: some subtlety around find, the escape key cancels it, but the menu
                    // shouldn't.
                    self.send_action("select_all");
                }
            }
        }
        true
    }

    fn key(&mut self, event: &KeyEvent, ctx: &mut HandlerCtx) -> bool {
        match event.key {
            KeyVariant::Vkey(vk) => return self.keydown(vk, event.mods, ctx),
            KeyVariant::Char(ch) => {
                self.char(ch as u32, event.mods);
            }
        }
        true
    }
}

impl EditView {
    pub fn new() -> Self {
        Self {
            view_id: None,
            line_cache: LineCache::new(),
            dwrite_factory: directwrite::Factory::new().unwrap(),
            resources: None,
            scroll_offset: 0.0,
            size: (0.0, 0.0),
            viewport: 0..0,
            core: Weak::new(),
            pending: vec![],
        }
    }

    pub fn ui(self, ctx: &mut Ui) -> Id {
        ctx.add(self, &[])
    }

    fn create_resources(&mut self, p: &mut PaintCtx) -> Resources {
        let rt = p.render_target();
        let text_format = TextFormat::create(&self.dwrite_factory)
            .with_family("Consolas")
            .with_size(15.0)
            .build()
            .unwrap();
        Resources {
            fg: SolidColorBrush::create(rt)
                .with_color(0x00f0_f0ea)
                .build()
                .unwrap(),
            bg: SolidColorBrush::create(rt)
                .with_color(0x0027_2822)
                .build()
                .unwrap(),
            sel: SolidColorBrush::create(rt)
                .with_color(0x0049_483e)
                .build()
                .unwrap(),
            text_format,
        }
    }

    // pub fn rebuild_resources(&mut self) {
    //     self.resources = None;
    // }

    // pub fn clear_line_cache(&mut self) {
    //     self.line_cache = LineCache::new();
    // }

    // signature will change when we start caching
    fn get_text_line(&self, line_num: usize) -> Option<TextLine> {
        self.line_cache.get_line(line_num).map(|line| {
            let format = &self.resources.as_ref().unwrap().text_format;
            TextLine::create_from_line(line, &self.dwrite_factory, format)
        })
    }

    pub fn apply_update(&mut self, update: &Value) {
        self.line_cache.apply_update(update);
        self.constrain_scroll();
    }

    pub fn char(&mut self, ch: u32, _mods: u32) {
        if let Some(c) = ::std::char::from_u32(ch) {
            if ch >= 0x20 {
                // Don't insert control characters
                let params = json!({"chars": c.to_string()});
                self.send_edit_cmd("insert", &params);
            }
        }
    }

    fn send_edit_cmd(&mut self, method: &str, params: &Value) {
        if let Some((core, view_id)) = self.core.upgrade().zip(self.view_id.as_ref()) {
            let edit_params = json!({
                "method": method,
                "params": params,
                "view_id": view_id,
            });
            core.lock().unwrap().send_notification("edit", &edit_params);
            // NOTE: For debugging, could be replaced by trace logging
            // println!("fe->core: {}", json!({
            //     "method": method,
            //     "params": params,
            // }));
        } else {
            self.pending.push((method.to_owned(), params.clone()));
        }
    }

    /// Sends a simple action with no parameters
    fn send_action(&mut self, method: &str) {
        self.send_edit_cmd(method, &json!([]));
    }

    pub fn keydown(&mut self, vk_code: i32, mods: u32, ctx: &mut HandlerCtx) -> bool {
        // Handle special keys here
        match vk_code {
            VK_RETURN => {
                // TODO(Olive): modifiers are variants of open
                self.send_action("insert_newline");
            }
            VK_TAB => {
                // TODO(Olive): modified versions
                self.send_action("insert_tab");
            }
            VK_UP => {
                if mods == M_CTRL {
                    self.scroll_offset -= LINE_SPACE;
                    self.constrain_scroll();
                    self.update_viewport();
                    ctx.invalidate();
                } else {
                    let action = if mods == M_CTRL | M_ALT {
                        "add_selection_above"
                    } else {
                        s(mods, "move_up", "move_up_and_modify_selection")
                    };
                    // TODO(Olive): swap line up is ctrl + shift
                    self.send_action(action);
                }
            }
            VK_DOWN => {
                if mods == M_CTRL {
                    self.scroll_offset += LINE_SPACE;
                    self.constrain_scroll();
                    self.update_viewport();
                    ctx.invalidate();
                } else {
                    let action = if mods == M_CTRL | M_ALT {
                        "add_selection_below"
                    } else {
                        s(mods, "move_down", "move_down_and_modify_selection")
                    };
                    self.send_action(action);
                }
            }
            VK_LEFT => {
                // TODO(Olive): there is a subtle distinction between alt and ctrl
                let action = if (mods & (M_ALT | M_CTRL)) == 0 {
                    s(mods, "move_left", "move_left_and_modify_selection")
                } else {
                    s(
                        mods,
                        "move_word_left",
                        "move_word_left_and_modify_selection",
                    )
                };
                self.send_action(action);
            }
            VK_RIGHT => {
                // TODO(Olive): there is a subtle distinction between alt and ctrl
                let action = if (mods & (M_ALT | M_CTRL)) == 0 {
                    s(mods, "move_right", "move_right_and_modify_selection")
                } else {
                    s(
                        mods,
                        "move_word_right",
                        "move_word_right_and_modify_selection",
                    )
                };
                self.send_action(action);
            }
            VK_PRIOR => {
                self.send_action(s(mods, "scroll_page_up", "page_up_and_modify_selection"));
            }
            VK_NEXT => {
                self.send_action(s(
                    mods,
                    "scroll_page_down",
                    "page_down_and_modify_selection",
                ));
            }
            VK_HOME => {
                let action = if (mods & M_CTRL) == 0 {
                    s(
                        mods,
                        "move_to_left_end_of_line",
                        "move_to_left_end_of_line_and_modify_selection",
                    )
                } else {
                    s(
                        mods,
                        "move_to_beginning_of_document",
                        "move_to_beginning_of_document_and_modify_selection",
                    )
                };
                self.send_action(action);
            }
            VK_END => {
                let action = if (mods & M_CTRL) == 0 {
                    s(
                        mods,
                        "move_to_right_end_of_line",
                        "move_to_right_end_of_line_and_modify_selection",
                    )
                } else {
                    s(
                        mods,
                        "move_to_end_of_document",
                        "move_to_end_of_document_and_modify_selection",
                    )
                };
                self.send_action(action);
            }
            VK_ESCAPE => {
                self.send_action("cancel_operation");
            }
            VK_BACK => {
                let action = if (mods & M_CTRL) == 0 {
                    "delete_backward"
                } else {
                    // should be "delete to beginning of paragraph" but not supported
                    s(mods, "delete_word_backward", "delete_to_beginning_of_line")
                };
                self.send_action(action);
            }
            VK_DELETE => {
                let action = if (mods & M_CTRL) == 0 {
                    // TODO(Olive): shift-delete should be "delete line"
                    "delete_forward"
                } else {
                    s(mods, "delete_word_forward", "delete_to_end_of_paragraph")
                };
                self.send_action(action);
            }
            VK_OEM_4 => {
                // generally '[' key, but might vary on non-US keyboards
                if mods == M_CTRL {
                    self.send_action("outdent");
                } else {
                    return false;
                }
            }
            VK_OEM_6 => {
                // generally ']' key, but might vary on non-US keyboards
                if mods == M_CTRL {
                    self.send_action("indent");
                } else {
                    return false;
                }
            }
            _ => return false,
        }
        true
    }

    // pub fn mouse_wheel(&mut self, delta: i32, _mods: u32) {
    //     // TODO(Olive): scale properly, taking SPI_GETWHEELSCROLLLINES into account
    //     let scroll_scaling = 0.5;
    //     self.scroll_offset -= (delta as f32) * scroll_scaling;
    //     self.constrain_scroll();
    //     self.update_viewport();
    // }

    fn constrain_scroll(&mut self) {
        let max_scroll =
            LINE_SPACE.mul_add(self.line_cache.height().saturating_sub(1) as f32, TOP_PAD);
        if self.scroll_offset < 0.0 {
            self.scroll_offset = 0.0;
        } else if self.scroll_offset > max_scroll {
            self.scroll_offset = max_scroll;
        }
    }

    // Takes y in screen-space px.
    fn y_to_line(&self, y: f32) -> usize {
        let mut line = (y + self.scroll_offset - TOP_PAD) / LINE_SPACE;
        if line < 0.0 {
            line = 0.0;
        }
        let line = line.floor() as usize;
        min(line, self.line_cache.height())
    }

    /// Takes x, y in screen-space px, returns line number and utf8 offset within line.
    fn xy_to_line_col(&self, x: f32, y: f32) -> (usize, usize) {
        let line_num = self.y_to_line(y);
        let col = if let (Some(textline), Some(line)) = (
            self.get_text_line(line_num),
            self.line_cache.get_line(line_num),
        ) {
            textline.hit_test(x - LEFT_PAD, 0.0, line.text())
        } else {
            0
        };
        (line_num, col)
    }

    fn update_viewport(&mut self) {
        let first_line = self.y_to_line(0.0);
        let last_line = first_line + ((self.size.1 / LINE_SPACE).floor() as usize) + 1;
        let viewport = first_line..last_line;
        if viewport != self.viewport {
            self.viewport = viewport;
            self.send_edit_cmd("scroll", &json!([first_line, last_line]));
        }
    }

    pub fn scroll_to(&mut self, line: usize) {
        let y = line_to_content_y(line);
        let bottom_slop = 20.0;
        if y < self.scroll_offset {
            self.scroll_offset = y;
        // TODO(Olive) Get rid of this second check to `y`? It's a float after all.
        } else if y > self.scroll_offset + self.size.1 - bottom_slop {
            self.scroll_offset = y - (self.size.1 - bottom_slop);
        }
    }
}

// Helper function for choosing between normal and shifted action
const fn s<'a>(mods: u32, normal: &'a str, shifted: &'a str) -> &'a str {
    if (mods & M_SHIFT) == 0 {
        normal
    } else {
        shifted
    }
}

/// Convert line number to y coordinate in content space.
fn line_to_content_y(line: usize) -> f32 {
    (line as f32).mul_add(LINE_SPACE, TOP_PAD)
}
