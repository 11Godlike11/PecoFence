//! A scoped presentation layer for native HMENU popups. USER32 still handles selection,
//! dismissal, keyboard navigation, accessibility and cascading menus; shell-owned drawing
//! is left alone. No Explorer hooks or private uxtheme ordinals are involved.

use crate::bindings::*;
use crate::tray::QuickAction;
use crate::wide::to_wide;
use std::cell::RefCell;
use windows_core::{BOOL, PCWSTR};

const OWNER_DRAW: u32 = 0x100;
const MIIM_DATA: u32 = 0x20;
const MIIM_SUBMENU: u32 = 4;
const MENU_CHAR: u32 = 0x120;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct MenuInfo {
    size: u32,
    mask: u32,
    style: u32,
    max_height: u32,
    background: HBRUSH,
    help_id: u32,
    data: usize,
}
#[repr(C)]
struct Measure {
    kind: u32,
    ctl: u32,
    id: u32,
    width: u32,
    height: u32,
    data: usize,
}
#[repr(C)]
struct Draw {
    kind: u32,
    ctl: u32,
    id: u32,
    action: u32,
    state: u32,
    hwnd: HWND,
    dc: HDC,
    rect: RECT,
    data: usize,
}
#[repr(C)]
#[derive(Default)]
struct HighContrast {
    size: u32,
    flags: u32,
    scheme: *mut u16,
}
#[repr(C)]
#[derive(Default)]
struct BitmapInfo {
    kind: i32,
    width: i32,
    height: i32,
    stride: i32,
    planes: u16,
    bits_per_pixel: u16,
    bits: *mut core::ffi::c_void,
}
windows_core::link!("user32.dll" "system" fn GetMenuItemInfoW(menu: HMENU, item: u32, by_position: BOOL, info: *mut MENUITEMINFOW) -> BOOL);
windows_core::link!("user32.dll" "system" fn GetMenuInfo(menu: HMENU, info: *mut MenuInfo) -> BOOL);
windows_core::link!("user32.dll" "system" fn SetMenuInfo(menu: HMENU, info: *const MenuInfo) -> BOOL);
windows_core::link!("user32.dll" "system" fn FillRect(dc: HDC, rect: *const RECT, brush: HBRUSH) -> i32);
windows_core::link!("user32.dll" "system" fn SystemParametersInfoW(action: u32, param: u32, data: *mut core::ffi::c_void, flags: u32) -> BOOL);
windows_core::link!("gdi32.dll" "system" fn SetBkMode(dc: HDC, mode: i32) -> i32);
windows_core::link!("gdi32.dll" "system" fn SaveDC(dc: HDC) -> i32);
windows_core::link!("gdi32.dll" "system" fn RestoreDC(dc: HDC, saved: i32) -> BOOL);
windows_core::link!("gdi32.dll" "system" fn CreatePen(style: i32, width: i32, color: COLORREF) -> HGDIOBJ);
windows_core::link!("gdi32.dll" "system" fn RoundRect(dc: HDC, left: i32, top: i32, right: i32, bottom: i32, width: i32, height: i32) -> BOOL);
windows_core::link!("msimg32.dll" "system" fn AlphaBlend(dest: HDC, x: i32, y: i32, width: i32, height: i32, source: HDC, sx: i32, sy: i32, sw: i32, sh: i32, blend: u32) -> BOOL);
windows_core::link!("uxtheme.dll" "system" fn SetWindowTheme(hwnd: HWND, app: PCWSTR, ids: PCWSTR) -> windows_core::HRESULT);
type FilterHook = *mut core::ffi::c_void;
type FilterProc = Option<unsafe extern "system" fn(i32, usize, isize) -> isize>;
windows_core::link!("user32.dll" "system" fn SetWindowsHookExW(kind: i32, proc: FilterProc, module: *mut core::ffi::c_void, thread: u32) -> FilterHook);
windows_core::link!("user32.dll" "system" fn UnhookWindowsHookEx(hook: FilterHook) -> BOOL);
windows_core::link!("user32.dll" "system" fn CallNextHookEx(hook: FilterHook, code: i32, wp: usize, lp: isize) -> isize);
windows_core::link!("user32.dll" "system" fn EndMenu() -> BOOL);
windows_core::link!("user32.dll" "system" fn GetMenuItemRect(hwnd: HWND, menu: HMENU, pos: u32, rect: *mut RECT) -> BOOL);
windows_core::link!("user32.dll" "system" fn GetMenuState(menu: HMENU, pos: u32, flags: u32) -> u32);
windows_core::link!("user32.dll" "system" fn InvalidateRect(hwnd: HWND, rect: *const RECT, erase: BOOL) -> BOOL);

struct Item {
    menu: HMENU,
    pos: u32,
    original: MENUITEMINFOW,
    label: String,
    shortcut: String,
    width: u32,
    separator: bool,
    toolbar: bool,
}

struct Skin {
    scale: f32,
    max_height: u32,
    dark: bool,
    font: HFONT,
    bold: HFONT,
    glyph: HFONT,
    background: HBRUSH,
    hover: HBRUSH,
    line: HBRUSH,
    hover_pen: HGDIOBJ,
    // Boxes keep itemData addresses stable while lazy shell submenus add more items.
    #[allow(clippy::vec_box)] // USER32 holds these addresses, including across Vec growth.
    items: Vec<Box<Item>>,
    menus: Vec<(HMENU, MenuInfo)>,
    root: HMENU,
    popup: HWND,
    actions: Vec<QuickAction>,
    action_hover: Option<usize>,
    action_command: Option<u32>,
}

thread_local! { static ACTIVE: RefCell<Option<Skin>> = const { RefCell::new(None) }; }

pub(crate) fn high_contrast() -> bool {
    let mut hc = HighContrast {
        size: size_of::<HighContrast>() as u32,
        ..Default::default()
    };
    // SAFETY: SPI_GETHIGHCONTRAST writes a HIGHCONTRASTW of the supplied size.
    unsafe {
        SystemParametersInfoW(0x42, hc.size, (&mut hc as *mut HighContrast).cast(), 0).as_bool()
            && hc.flags & 1 != 0
    }
}

pub(crate) fn uses_dark_colors() -> bool {
    !high_contrast() && !crate::theme::apps_use_light_theme()
}

fn color(light: u32, dark: u32, is_dark: bool) -> u32 {
    if is_dark { dark } else { light }
}

impl Skin {
    fn px(&self, value: f32) -> i32 {
        (value * self.scale).round() as i32
    }

    fn item(&self, data: usize) -> Option<&Item> {
        // Do not dereference unknown shell itemData pointers.
        self.items
            .iter()
            .find(|i| (&***i as *const Item) as usize == data)
            .map(|i| &**i)
    }

    fn prepare(&mut self, menu: HMENU) {
        if menu.0.is_null() {
            return;
        }
        // SAFETY: USER32 supplied a live popup handle. All attached data remains alive until
        // TrackPopupMenuEx returns, at which point original fields and brushes are restored.
        unsafe {
            if !self.menus.iter().any(|(h, _)| *h == menu) {
                let mut old = MenuInfo {
                    size: size_of::<MenuInfo>() as u32,
                    mask: 0x13,
                    ..Default::default()
                };
                if GetMenuInfo(menu, &mut old).as_bool() {
                    let styled = MenuInfo {
                        background: self.background,
                        style: old.style | 0x8000_0000,
                        max_height: self.max_height,
                        ..old
                    };
                    let _ = SetMenuInfo(menu, &styled);
                    self.menus.push((menu, old));
                }
            }
            let dc = GetDC(None);
            let old_font = SelectObject(dc, HGDIOBJ(self.font.0));
            let mut added = Vec::new();
            let mut width = self.px(if menu == self.root && !self.actions.is_empty() {
                216.0
            } else {
                184.0
            }) as u32;
            for pos in 0..GetMenuItemCount(Some(menu)).max(0) as u32 {
                let mut text = [0u16; 1024];
                let mut info = MENUITEMINFOW {
                    cbSize: size_of::<MENUITEMINFOW>() as u32,
                    fMask: MIIM_FTYPE as u32
                        | MIIM_STRING as u32
                        | MIIM_STATE as u32
                        | MIIM_ID as u32
                        | MIIM_BITMAP as u32
                        | MIIM_DATA
                        | MIIM_SUBMENU,
                    dwTypeData: windows_core::PWSTR(text.as_mut_ptr()),
                    cch: text.len() as u32,
                    ..Default::default()
                };
                if !GetMenuItemInfoW(menu, pos, BOOL(1), &mut info).as_bool() {
                    continue;
                }
                if info.fType & OWNER_DRAW != 0 {
                    if let Some(it) = self.item(info.dwItemData) {
                        width = width.max(it.width);
                    }
                    // A shell extension's own renderer and itemData are never replaced.
                    continue;
                }
                let raw = String::from_utf16_lossy(&text[..(info.cch as usize).min(text.len())]);
                let (label, shortcut) = raw.split_once('\t').unwrap_or((&raw, ""));
                let mut measure = RECT::default();
                let wide = to_wide(&raw.replace('\t', "    "));
                let _ = DrawTextW(
                    dc,
                    PCWSTR(wide.as_ptr()),
                    -1,
                    &mut measure,
                    0x400 | DT_SINGLELINE as u32,
                );
                let toolbar = menu == self.root && pos == 0 && !self.actions.is_empty();
                if !toolbar {
                    width = width.max((measure.right - measure.left + self.px(62.0)).max(0) as u32);
                }
                info.dwTypeData = windows_core::PWSTR::null();
                info.cch = 0;
                let mut item = Box::new(Item {
                    menu,
                    pos,
                    original: info,
                    label: label.into(),
                    shortcut: shortcut.into(),
                    width: 0,
                    separator: info.fType & MFT_SEPARATOR as u32 != 0,
                    toolbar,
                });
                let data = (&mut *item as *mut Item) as usize;
                let change = MENUITEMINFOW {
                    cbSize: size_of::<MENUITEMINFOW>() as u32,
                    // A non-null hbmpItem makes USER32 use its bitmap/string renderer even
                    // with MFT_OWNERDRAW. We draw that saved bitmap ourselves instead.
                    fMask: MIIM_FTYPE as u32 | MIIM_DATA | MIIM_BITMAP as u32,
                    fType: (info.fType & !4) | OWNER_DRAW,
                    hbmpItem: HBITMAP::default(),
                    dwItemData: data,
                    ..Default::default()
                };
                if SetMenuItemInfoW(menu, pos, true, &change).as_bool() {
                    added.push(item);
                }
            }
            let _ = SelectObject(dc, old_font);
            let _ = ReleaseDC(None, dc);
            for item in &mut added {
                item.width = width.min(self.px(280.0) as u32);
            }
            self.items.extend(added);
        }
    }

    fn draw(&self, draw: &Draw, item: &Item) {
        // SAFETY: DRAWITEMSTRUCT gives us a valid DC and item rectangle for this callback.
        // SaveDC/RestoreDC preserve the caller's complete GDI state, including font and clip.
        unsafe {
            let saved = SaveDC(draw.dc);
            let _ = FillRect(draw.dc, &draw.rect, self.background);
            let mut r = draw.rect;
            if item.toolbar {
                let count = self.actions.len() as i32;
                let pad = self.px(8.0);
                let step = (r.right - r.left - pad * 2) / count;
                let hovered = self.action_hover.or_else(|| {
                    (draw.state & 1 != 0)
                        .then(|| self.actions.iter().position(|a| !a.disabled))
                        .flatten()
                });
                for (i, action) in self.actions.iter().enumerate() {
                    let left = r.left + pad + i as i32 * step;
                    if hovered == Some(i) && !action.disabled {
                        let _ = SelectObject(draw.dc, HGDIOBJ(self.hover.0));
                        let _ = SelectObject(draw.dc, self.hover_pen);
                        let round = self.px(8.0);
                        let _ = RoundRect(
                            draw.dc,
                            left + self.px(4.0),
                            r.top + self.px(4.0),
                            left + step - self.px(4.0),
                            r.bottom - self.px(4.0),
                            round,
                            round,
                        );
                    }
                    let _ = SetBkMode(draw.dc, 1);
                    let _ = SetTextColor(
                        draw.dc,
                        COLORREF(if action.disabled {
                            color(0x999999, 0x808080, self.dark)
                        } else {
                            color(0x1b1b1b, 0xf5f5f5, self.dark)
                        }),
                    );
                    let _ = SelectObject(draw.dc, HGDIOBJ(self.glyph.0));
                    let glyph = to_wide(&action.glyph.to_string());
                    let mut box_ = RECT {
                        left,
                        right: left + step,
                        ..r
                    };
                    let _ = DrawTextW(
                        draw.dc,
                        PCWSTR(glyph.as_ptr()),
                        -1,
                        &mut box_,
                        (DT_CENTER | DT_VCENTER | DT_SINGLELINE) as u32,
                    );
                }
            } else if item.separator {
                let line = RECT {
                    left: r.left + self.px(12.0),
                    right: r.right - self.px(12.0),
                    top: (r.top + r.bottom) / 2,
                    bottom: (r.top + r.bottom) / 2 + 1,
                };
                let _ = FillRect(draw.dc, &line, self.line);
            } else {
                let disabled = draw.state & (0x2 | 0x4) != 0;
                if draw.state & 1 != 0 && !disabled {
                    let _ = SelectObject(draw.dc, HGDIOBJ(self.hover.0));
                    let _ = SelectObject(draw.dc, HGDIOBJ(self.hover_pen.0));
                    let pad = self.px(3.0);
                    let round = self.px(8.0);
                    let _ = RoundRect(
                        draw.dc,
                        r.left + pad,
                        r.top + self.px(1.0),
                        r.right - pad,
                        r.bottom - self.px(1.0),
                        round,
                        round,
                    );
                }
                let _ = SetBkMode(draw.dc, 1);
                let fg = if disabled {
                    color(0x999999, 0x808080, self.dark)
                } else {
                    color(0x1b1b1b, 0xf5f5f5, self.dark)
                };
                let _ = SetTextColor(draw.dc, COLORREF(fg));
                let _ = SelectObject(draw.dc, HGDIOBJ(self.glyph.0));
                let mut icon = RECT {
                    left: r.left + self.px(10.0),
                    right: r.left + self.px(28.0),
                    ..r
                };
                if draw.state & 8 != 0 {
                    let wide = to_wide("\u{E73E}");
                    let _ = DrawTextW(
                        draw.dc,
                        PCWSTR(wide.as_ptr()),
                        -1,
                        &mut icon,
                        (DT_CENTER | DT_SINGLELINE | DT_VCENTER) as u32,
                    );
                } else if item.original.hbmpItem.0 as usize > 16 {
                    let mut bm = BitmapInfo::default();
                    if GetObjectW(
                        HANDLE(item.original.hbmpItem.0),
                        size_of::<BitmapInfo>() as i32,
                        Some((&mut bm as *mut BitmapInfo).cast()),
                    ) > 0
                        && bm.width > 0
                        && bm.height > 0
                    {
                        let mem = CreateCompatibleDC(Some(draw.dc));
                        let prev = SelectObject(mem, HGDIOBJ(item.original.hbmpItem.0));
                        let size = self.px(16.0);
                        let _ = AlphaBlend(
                            draw.dc,
                            icon.left,
                            (r.top + r.bottom - size) / 2,
                            size,
                            size,
                            mem,
                            0,
                            0,
                            bm.width,
                            bm.height,
                            0x01ff0000,
                        );
                        let _ = SelectObject(mem, prev);
                        let _ = DeleteDC(mem);
                    }
                }
                // USER32 draws the submenu arrow; leave its reserved right-hand gutter free.
                let _ = SelectObject(
                    draw.dc,
                    HGDIOBJ(if draw.state & 0x1000 != 0 {
                        self.bold.0
                    } else {
                        self.font.0
                    }),
                );
                r.left += self.px(32.0);
                r.right -= self.px(16.0);
                let mut shortcut = r;
                if !item.shortcut.is_empty() {
                    let wide = to_wide(&item.shortcut);
                    let _ = SetTextColor(draw.dc, COLORREF(color(0x707070, 0xa6a6a6, self.dark)));
                    let _ = DrawTextW(
                        draw.dc,
                        PCWSTR(wide.as_ptr()),
                        -1,
                        &mut shortcut,
                        (DT_SINGLELINE | DT_VCENTER) as u32 | 2 | DT_NOPREFIX as u32,
                    );
                    let mut measure = RECT::default();
                    let _ = DrawTextW(
                        draw.dc,
                        PCWSTR(wide.as_ptr()),
                        -1,
                        &mut measure,
                        0x400 | DT_SINGLELINE as u32,
                    );
                    r.right -= measure.right + self.px(16.0);
                }
                let _ = SetTextColor(draw.dc, COLORREF(fg));
                let wide = to_wide(&item.label);
                let hide_prefix = if draw.state & 0x100 != 0 {
                    0x00100000
                } else {
                    0
                };
                let _ = DrawTextW(
                    draw.dc,
                    PCWSTR(wide.as_ptr()),
                    -1,
                    &mut r,
                    (DT_SINGLELINE | DT_VCENTER) as u32 | 0x8000 | hide_prefix,
                );
            }
            let _ = RestoreDC(draw.dc, saved);
        }
    }
}

/// Only filters this UI thread's menu loop. Toolbar commands finish the same native menu
/// transaction as ordinary rows; no process-wide mouse hook or second popup loop is used.
unsafe extern "system" fn menu_filter(code: i32, wp: usize, lp: isize) -> isize {
    let mut finish = false;
    let mut repaint = HWND::default();
    let handled = if code == 2 && lp != 0 {
        let msg = unsafe { &*(lp as *const MSG) };
        ACTIVE.with(|a| {
            let mut guard = a.borrow_mut();
            let Some(s) = guard.as_mut() else {
                return false;
            };
            if s.actions.is_empty() || s.popup.0.is_null() {
                return false;
            }
            let mut rect = RECT::default();
            if !unsafe { GetMenuItemRect(HWND::default(), s.root, 0, &mut rect) }.as_bool() {
                return false;
            }
            match msg.message {
                0x0200..=0x0202 => {
                    let p = msg.pt;
                    let inside = p.x >= rect.left
                        && p.x < rect.right
                        && p.y >= rect.top
                        && p.y < rect.bottom;
                    let hover = if inside {
                        let pad = s.px(8.0);
                        let step = (rect.right - rect.left - pad * 2) / s.actions.len() as i32;
                        let x = p.x - rect.left - pad;
                        (x >= 0 && x < step * s.actions.len() as i32)
                            .then_some((x.max(0) / step.max(1)) as usize)
                    } else {
                        None
                    };
                    if s.action_hover != hover {
                        s.action_hover = hover;
                        repaint = s.popup;
                    }
                    if inside && msg.message == 0x0202 {
                        if let Some(i) = hover
                            && !s.actions[i].disabled
                        {
                            s.action_command = Some(s.actions[i].id);
                            finish = true;
                        }
                        return true;
                    }
                    inside && msg.message == 0x0201
                }
                0x0100 if unsafe { GetMenuState(s.root, 0, 0x400) } & 0x80 != 0 => {
                    match msg.wParam.0 as u32 {
                        0x25 | 0x27 => {
                            let n = s.actions.len();
                            let direction = if msg.wParam.0 == 0x25 { n - 1 } else { 1 };
                            let first = s.action_hover.unwrap_or_else(|| {
                                s.actions.iter().position(|a| !a.disabled).unwrap_or(0)
                            });
                            for step in 1..=n {
                                let i = (first + direction * step) % n;
                                if !s.actions[i].disabled {
                                    s.action_hover = Some(i);
                                    break;
                                }
                            }
                            repaint = s.popup;
                            true
                        }
                        0x0D | 0x20 => {
                            let selected = s
                                .action_hover
                                .or_else(|| s.actions.iter().position(|a| !a.disabled));
                            if let Some(i) = selected
                                && !s.actions[i].disabled
                            {
                                s.action_command = Some(s.actions[i].id);
                                finish = true;
                            }
                            true
                        }
                        _ => false,
                    }
                }
                _ => false,
            }
        })
    } else {
        false
    };
    if !repaint.0.is_null() {
        let _ = unsafe { InvalidateRect(repaint, core::ptr::null(), BOOL(0)) };
    }
    if finish {
        let _ = unsafe { EndMenu() };
    }
    if handled {
        1
    } else {
        unsafe { CallNextHookEx(core::ptr::null_mut(), code, wp, lp) }
    }
}

pub(crate) struct Session {
    hook: HWINEVENTHOOK,
    filter: FilterHook,
}

impl Session {
    #[cfg(test)]
    pub(crate) fn begin(menu: HMENU, owner: HWND) -> Option<Self> {
        let point = crate::window::cursor_pos();
        Self::begin_with_actions(menu, owner, &[], (point.x, point.y))
    }

    pub(crate) fn command(&self) -> Option<u32> {
        ACTIVE.with(|a| a.borrow().as_ref().and_then(|s| s.action_command))
    }

    pub(crate) fn begin_with_actions(
        menu: HMENU,
        owner: HWND,
        actions: &[QuickAction],
        point: (i32, i32),
    ) -> Option<Self> {
        let contrast = high_contrast();
        if contrast || ACTIVE.with(|a| a.borrow().is_some()) {
            tracing::debug!(contrast, "native menu presentation skipped");
            return None;
        }
        // SAFETY: fonts and brushes are GDI resources owned by this scoped session.
        unsafe {
            let scale = GetDpiForWindow(owner).max(96) as f32 / 96.0;
            let font = |name: &str, weight: i32| {
                let face = to_wide(name);
                CreateFontW(
                    -(13.0 * scale).round() as i32,
                    0,
                    0,
                    0,
                    weight,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET as u32,
                    0,
                    0,
                    CLEARTYPE_QUALITY as u32,
                    0,
                    PCWSTR(face.as_ptr()),
                )
            };
            let dark = uses_dark_colors();
            let hover = color(0xefefef, 0x383838, dark);
            let mut skin = Skin {
                scale,
                max_height: crate::monitors::query(crate::monitors::monitor_from_point(
                    point.0, point.1,
                ))
                .map(|m| {
                    ((m.work_area.bottom - m.work_area.top) as f32 * 0.7)
                        .min(300.0 * scale)
                        .max(1.0) as u32
                })
                .unwrap_or((300.0 * scale) as u32),
                dark,
                font: font("Microsoft YaHei UI", FW_NORMAL),
                bold: font("Microsoft YaHei UI", FW_SEMIBOLD),
                glyph: font("Segoe Fluent Icons", FW_NORMAL),
                background: CreateSolidBrush(COLORREF(color(0xf9f9f9, 0x2b2b2b, dark))),
                hover: CreateSolidBrush(COLORREF(hover)),
                line: CreateSolidBrush(COLORREF(color(0xe3e3e3, 0x424242, dark))),
                hover_pen: CreatePen(0, 1, COLORREF(hover)),
                items: Vec::new(),
                menus: Vec::new(),
                root: menu,
                popup: HWND::default(),
                actions: actions.to_vec(),
                action_hover: None,
                action_command: None,
            };
            skin.prepare(menu);
            tracing::info!(
                scale,
                dark,
                items = skin.items.len(),
                "native menu presentation prepared"
            );
            ACTIVE.with(|a| *a.borrow_mut() = Some(skin));
            let hook = SetWinEventHook(
                6,
                6,
                None,
                Some(popup_started),
                GetCurrentProcessId(),
                GetCurrentThreadId(),
                WINEVENT_OUTOFCONTEXT as u32,
            );
            let filter = if actions.is_empty() {
                core::ptr::null_mut()
            } else {
                SetWindowsHookExW(
                    -1,
                    Some(menu_filter),
                    core::ptr::null_mut(),
                    GetCurrentThreadId(),
                )
            };
            Some(Self { hook, filter })
        }
    }
}

unsafe extern "system" fn popup_started(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    if hwnd.0.is_null() {
        return;
    }
    let dark = ACTIVE.with(|a| a.borrow().as_ref().map(|s| s.dark));
    if let Some(dark) = dark {
        let menu = unsafe { SendMessageW(hwnd, 0x01E1, WPARAM(0), LPARAM(0)) };
        ACTIVE.with(|a| {
            if let Some(s) = a.borrow_mut().as_mut()
                && s.root.0 as isize == menu.0
            {
                s.popup = hwnd;
            }
        });
        // SAFETY: the in-process, UI-thread-scoped WinEvent callback supplied the popup HWND.
        // Disabling its legacy theme lets MENUINFO's brush also cover the outer padding.
        unsafe {
            let empty = to_wide("");
            let _ = SetWindowTheme(hwnd, PCWSTR(empty.as_ptr()), PCWSTR(empty.as_ptr()));
        }
        let _ = crate::dwm::set_immersive_dark_mode(hwnd, dark);
        let _ = crate::dwm::set_corner_preference(hwnd, crate::dwm::CornerPreference::RoundSmall);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: stop callbacks before releasing the menu's attached data and GDI resources.
        unsafe {
            if !self.filter.is_null() {
                let _ = UnhookWindowsHookEx(self.filter);
            }
            if !self.hook.0.is_null() {
                let _ = UnhookWinEvent(self.hook);
            }
        }
        ACTIVE.with(|a| {
            if let Some(skin) = a.borrow_mut().take() {
                // SAFETY: the HMENU is owned by the still-live PopupMenu. A lazy extension can
                // remove entries, so restore only entries still carrying our exact data pointer.
                unsafe {
                    for item in &skin.items {
                        let mut now = MENUITEMINFOW {
                            cbSize: size_of::<MENUITEMINFOW>() as u32,
                            fMask: MIIM_DATA,
                            ..Default::default()
                        };
                        let current_pos = (0..GetMenuItemCount(Some(item.menu)).max(0) as u32)
                            .find(|pos| {
                                GetMenuItemInfoW(item.menu, *pos, BOOL(1), &mut now).as_bool()
                                    && now.dwItemData == (&**item as *const Item) as usize
                            });
                        if let Some(pos) = current_pos {
                            let old = MENUITEMINFOW {
                                cbSize: size_of::<MENUITEMINFOW>() as u32,
                                fMask: MIIM_FTYPE as u32 | MIIM_DATA | MIIM_BITMAP as u32,
                                fType: item.original.fType,
                                dwItemData: item.original.dwItemData,
                                hbmpItem: item.original.hbmpItem,
                                ..Default::default()
                            };
                            let _ = SetMenuItemInfoW(item.menu, pos, true, &old);
                        }
                    }
                    for (menu, old) in &skin.menus {
                        let _ = SetMenuInfo(*menu, old);
                    }
                    for handle in [
                        skin.font.0,
                        skin.bold.0,
                        skin.glyph.0,
                        skin.background.0,
                        skin.hover.0,
                        skin.line.0,
                        skin.hover_pen.0,
                    ] {
                        let _ = DeleteObject(HGDIOBJ(handle));
                    }
                }
            }
        });
    }
}

/// Call after the shell has filled a lazy submenu, before USER32 measures it.
pub(crate) fn prepare_popup(menu: HMENU) {
    ACTIVE.with(|a| {
        if let Some(s) = a.borrow_mut().as_mut() {
            s.prepare(menu);
        }
    });
}

pub(crate) fn forward(message: u32, wparam: usize, lparam: isize) -> Option<isize> {
    ACTIVE.with(|a| {
        let guard = a.borrow();
        let skin = guard.as_ref()?;
        // SAFETY: these messages carry the documented structs valid for the callback. Shell
        // data is matched by pointer value against our boxes before any item is accessed.
        unsafe {
            match message {
                m if m == WM_MEASUREITEM as u32 && lparam != 0 => {
                    let measure = &mut *(lparam as *mut Measure);
                    if measure.kind != 1 {
                        return None;
                    }
                    let item = skin.item(measure.data)?;
                    tracing::debug!(
                        data = measure.data,
                        scale = skin.scale,
                        "measuring styled menu item"
                    );
                    measure.width = item.width;
                    measure.height = skin.px(if item.toolbar {
                        38.0
                    } else if item.separator {
                        7.0
                    } else {
                        28.0
                    }) as u32;
                    Some(1)
                }
                m if m == WM_DRAWITEM as u32 && lparam != 0 => {
                    let draw = &*(lparam as *const Draw);
                    if draw.kind != 1 {
                        return None;
                    }
                    let item = skin.item(draw.data)?;
                    skin.draw(draw, item);
                    Some(1)
                }
                MENU_CHAR => {
                    let menu = HMENU(lparam as *mut core::ffi::c_void);
                    let ch = char::from_u32((wparam & 0xffff) as u32)?.to_ascii_lowercase();
                    let matches: Vec<_> = skin
                        .items
                        .iter()
                        .filter(|i| {
                            i.menu == menu
                                && i.original.fState & 3 == 0
                                && mnemonic(&i.label) == Some(ch)
                        })
                        .collect();
                    if matches.len() == 1 {
                        Some((3 << 16) | matches[0].pos as isize)
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
    })
}

fn mnemonic(label: &str) -> Option<char> {
    let mut chars = label.chars();
    while let Some(ch) = chars.next() {
        if ch == '&' {
            let next = chars.next()?;
            if next != '&' {
                return Some(next.to_ascii_lowercase());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tray::PopupMenu;

    #[test]
    fn scoped_skin_preserves_strings_state_and_shell_item_data() {
        if high_contrast() {
            return;
        }
        let menu = PopupMenu::new();
        menu.item(1, "&Open\tEnter", false, false)
            .icon('\u{E8A5}')
            .item(2, "Disabled", true, true);
        // A shell extension's existing owner-draw data must not be treated as our own.
        menu.item(3, "Extension", false, false);
        let extension = MENUITEMINFOW {
            cbSize: size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_FTYPE as u32 | MIIM_DATA,
            fType: OWNER_DRAW,
            dwItemData: 0x1234,
            ..Default::default()
        };
        // SAFETY: menu and all data used here are live for the entire scope.
        unsafe {
            assert!(SetMenuItemInfoW(menu.handle(), 2, true, &extension).as_bool());
        }
        let labels = menu.labels();
        let session = Session::begin(menu.handle(), HWND::default()).unwrap();
        assert_eq!(menu.labels(), labels);
        let info = |pos| {
            let mut info = MENUITEMINFOW {
                cbSize: size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_FTYPE as u32 | MIIM_DATA | MIIM_STATE as u32,
                ..Default::default()
            };
            // SAFETY: menu is live; `info` is a sized output buffer.
            unsafe {
                assert!(GetMenuItemInfoW(menu.handle(), pos, BOOL(1), &mut info).as_bool());
            }
            info
        };
        assert_eq!(info(2).dwItemData, 0x1234);
        assert_eq!(info(1).fState & (8 | 3), 8 | 3);
        let own = info(0);
        let mut measure = Measure {
            kind: 1,
            ctl: 0,
            id: 1,
            width: 0,
            height: 0,
            data: own.dwItemData,
        };
        assert_eq!(
            forward(
                WM_MEASUREITEM as u32,
                0,
                (&mut measure as *mut Measure) as isize
            ),
            Some(1)
        );
        assert_eq!(measure.height, 28);
        measure.data = 0x1234;
        assert_eq!(
            forward(
                WM_MEASUREITEM as u32,
                0,
                (&mut measure as *mut Measure) as isize
            ),
            None
        );
        drop(session);
        assert_eq!(info(0).fType & OWNER_DRAW, 0);
        assert_eq!(info(0).dwItemData, 0);
        assert_eq!(info(2).dwItemData, 0x1234);
        assert_eq!(menu.labels(), labels);
        assert!(ACTIVE.with(|a| a.borrow().is_none()));
    }
}
