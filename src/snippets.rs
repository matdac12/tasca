//! Saved text. The bookmark at the top-right of the History panel opens this
//! list. The file is `%LOCALAPPDATA%\Tasca\snippets.txt`, written on a worker
//! the same way settings are: temp file, then rename. A missing or unreadable
//! file is an empty list.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{GlobalFree, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetStockObject, COLOR_BTNFACE, DEFAULT_GUI_FONT, HBRUSH};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::log::log;
use crate::proto::WM_SNIPPETS_LOADED;

const MAGIC: &str = "TSCS\n";
const SEP: &str = "\n\u{1e}\n";

const ID_SAVE: i32 = 1;
const ID_CLOSE: i32 = 2;
const ID_NEW: i32 = 10;
const ID_DELETE: i32 = 11;
const ID_COPY: i32 = 12;
const ID_LIST: i32 = 100;
const ID_EDIT: i32 = 101;

const LB_ADDSTRING: u32 = 0x0180;
const LB_RESETCONTENT: u32 = 0x0184;
const LB_SETCURSEL: u32 = 0x0186;
const LB_GETCURSEL: u32 = 0x0188;
const LBN_SELCHANGE: u32 = 1;
const LBN_DBLCLK: u32 = 2;

static LIST: Mutex<Vec<String>> = Mutex::new(Vec::new());
static LOADED: Mutex<Option<Vec<String>>> = Mutex::new(None);
static DIRTY: AtomicBool = AtomicBool::new(false);
static TX: Mutex<Option<Sender<Job>>> = Mutex::new(None);

thread_local! {
    static OPEN: Cell<bool> = const { Cell::new(false) };
    static DONE: Cell<bool> = const { Cell::new(false) };
    static UI: RefCell<Option<Dialog>> = const { RefCell::new(None) };
}

struct Dialog {
    hwnd: HWND,
    list: HWND,
    edit: HWND,
}

enum Job {
    /// The resident window, as an integer so the job can cross threads.
    Load(isize),
    Save(Vec<String>),
    /// Reply once every earlier save has been written.
    Flush(Sender<()>),
}

/// Text worth keeping. Trailing space goes; a blank body is refused.
pub fn accept(text: &str) -> Option<String> {
    let body = text.trim_end();
    if body.trim().is_empty() {
        None
    } else {
        Some(body.to_string())
    }
}

/// Put `body` first. The same text (ignoring surrounding space) is not kept twice.
pub fn remember(list: &mut Vec<String>, body: String) {
    let key = body.trim();
    list.retain(|s| s.trim() != key);
    list.insert(0, body);
}

fn encode(items: &[String]) -> String {
    let mut out = String::from(MAGIC);
    for (i, body) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(SEP);
        }
        out.push_str(body);
    }
    out
}

fn decode(text: &str) -> Vec<String> {
    let Some(rest) = text.strip_prefix(MAGIC) else {
        return Vec::new();
    };
    if rest.is_empty() {
        return Vec::new();
    }
    rest.split(SEP).filter_map(accept).collect()
}

fn load_from(path: &Path) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => decode(&text),
        Err(_) => Vec::new(),
    }
}

fn write_file(path: &Path, items: &[String]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("txt.tmp");
    std::fs::write(&tmp, encode(items))?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn file_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Tasca").join("snippets.txt"))
}

/// Start the worker and read the file. No file location: the list stays in memory.
pub fn create(resident: HWND) {
    let Some(path) = file_path() else {
        log("snippets: memory only");
        return;
    };
    let (tx, rx) = channel();
    *TX.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    thread::spawn(move || worker(path, rx));
    send(Job::Load(resident.0 as isize));
}

pub fn destroy() {
    let tx = TX.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(tx) = tx else { return };
    let (done_tx, done_rx) = channel();
    if tx.send(Job::Flush(done_tx)).is_ok() {
        let _ = done_rx.recv_timeout(Duration::from_secs(2));
    }
}

/// `WM_SNIPPETS_LOADED`. `None` when the message is not ours.
pub fn handle(msg: u32) -> Option<LRESULT> {
    if msg != WM_SNIPPETS_LOADED {
        return None;
    }
    on_loaded();
    Some(LRESULT(0))
}

/// The list window is up. The History must not close on the deactivation it causes.
pub fn is_open() -> bool {
    OPEN.get()
}

/// Keep `text` (from a History row, or anywhere else). Blank text is ignored.
pub fn save_text(text: String) {
    let Some(body) = accept(&text) else { return };
    let snapshot = {
        let mut list = LIST.lock().unwrap_or_else(|e| e.into_inner());
        remember(&mut list, body);
        DIRTY.store(true, Ordering::Relaxed);
        list.clone()
    };
    send(Job::Save(snapshot));
    log("snippets: saved");
    refresh(Some(0));
}

/// The list window, over `owner`. Returns when it closes.
pub fn open(owner: HWND) {
    if OPEN.get() {
        return;
    }
    OPEN.set(true);
    DONE.set(false);
    unsafe { run(owner) };
    OPEN.set(false);
}

fn send(job: Job) {
    let guard = TX.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(tx) = guard.as_ref() {
        let _ = tx.send(job);
    }
}

fn worker(path: PathBuf, rx: Receiver<Job>) {
    while let Ok(job) = rx.recv() {
        match job {
            Job::Load(owner) => {
                *LOADED.lock().unwrap_or_else(|e| e.into_inner()) = Some(load_from(&path));
                let owner = HWND(owner as *mut _);
                unsafe {
                    let _ = PostMessageW(Some(owner), WM_SNIPPETS_LOADED, WPARAM(0), LPARAM(0));
                }
            }
            Job::Save(items) => {
                if let Err(e) = write_file(&path, &items) {
                    log(format!("snippets: save failed: {e}"));
                }
            }
            Job::Flush(done) => {
                let _ = done.send(());
                break;
            }
        }
    }
}

fn on_loaded() {
    let Some(saved) = LOADED.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    let n = {
        let mut list = LIST.lock().unwrap_or_else(|e| e.into_inner());
        if DIRTY.load(Ordering::Relaxed) {
            for body in saved {
                if !list.iter().any(|s| s.trim() == body.trim()) {
                    list.push(body);
                }
            }
        } else {
            *list = saved;
        }
        list.len()
    };
    log(format!("snippets: loaded {n}"));
    refresh(None);
}

fn bodies() -> Vec<String> {
    LIST.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn first_line(body: &str) -> String {
    let line = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut short: String = line.chars().take(80).collect();
    if line.chars().count() > 80 {
        short.push('…');
    }
    short
}

fn to_edit(s: &str) -> String {
    s.replace('\n', "\r\n")
}

fn from_edit(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

fn refresh(select: Option<usize>) {
    let Some((list, edit)) = UI.with(|u| u.borrow().as_ref().map(|d| (d.list, d.edit))) else {
        return;
    };
    let items = bodies();
    unsafe {
        sm(list, LB_RESETCONTENT, 0, 0);
        for body in &items {
            let label = HSTRING::from(first_line(body));
            sm(list, LB_ADDSTRING, 0, label.as_ptr() as isize);
        }
        if let Some(i) = select {
            sm(list, LB_SETCURSEL, i, 0);
            if let Some(body) = items.get(i) {
                set_text(edit, &to_edit(body));
            }
        }
    }
}

fn selected() -> Option<usize> {
    let list = UI.with(|u| u.borrow().as_ref().map(|d| d.list))?;
    let i = unsafe { sm(list, LB_GETCURSEL, 0, 0).0 };
    (i >= 0).then_some(i as usize)
}

fn edit_text() -> String {
    let Some(edit) = UI.with(|u| u.borrow().as_ref().map(|d| d.edit)) else {
        return String::new();
    };
    from_edit(&window_text(edit))
}

fn window_text(hwnd: HWND) -> String {
    unsafe {
        let n = GetWindowTextLengthW(hwnd);
        if n <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; n as usize + 1];
        let got = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..got as usize])
    }
}

fn set_text(hwnd: HWND, text: &str) {
    let h = HSTRING::from(text);
    unsafe {
        let _ = SetWindowTextW(hwnd, &h);
    }
}

fn mutate(select: Option<usize>, f: impl FnOnce(&mut Vec<String>)) {
    let snapshot = {
        let mut list = LIST.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut list);
        DIRTY.store(true, Ordering::Relaxed);
        list.clone()
    };
    send(Job::Save(snapshot));
    refresh(select);
}

fn on_save() {
    let Some(body) = accept(&edit_text()) else {
        return;
    };
    let was = selected();
    mutate(Some(0), |list| {
        if let Some(i) = was {
            if i < list.len() {
                list.remove(i);
            }
        }
        remember(list, body);
    });
}

fn on_delete() {
    let Some(i) = selected() else { return };
    mutate(None, |list| {
        if i < list.len() {
            list.remove(i);
        }
    });
    if let Some(edit) = UI.with(|u| u.borrow().as_ref().map(|d| d.edit)) {
        set_text(edit, "");
    }
}

fn on_new() {
    if let Some((list, edit)) = UI.with(|u| u.borrow().as_ref().map(|d| (d.list, d.edit))) {
        unsafe {
            sm(list, LB_SETCURSEL, -1i32 as usize, 0);
            set_text(edit, "");
            let _ = SetFocus(Some(edit));
        }
    }
}

fn on_copy() {
    let text = accept(&edit_text()).or_else(|| selected().and_then(|i| bodies().get(i).cloned()));
    let Some(text) = text else { return };
    let hwnd = UI
        .with(|u| u.borrow().as_ref().map(|d| d.hwnd))
        .unwrap_or_default();
    if copy_text(hwnd, &text) {
        log("snippets: copied");
    } else {
        log("snippets: copy failed");
    }
}

fn copy_text(owner: HWND, text: &str) -> bool {
    let bytes: Vec<u8> = text
        .encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    unsafe {
        if OpenClipboard(Some(owner)).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        let mem = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
        let ok = match mem {
            Ok(mem) => {
                let p = GlobalLock(mem) as *mut u8;
                if p.is_null() {
                    let _ = GlobalFree(Some(mem));
                    false
                } else {
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
                    let _ = GlobalUnlock(mem);
                    if SetClipboardData(
                        CF_UNICODETEXT.0 as u32,
                        Some(windows::Win32::Foundation::HANDLE(mem.0)),
                    )
                    .is_ok()
                    {
                        true
                    } else {
                        let _ = GlobalFree(Some(mem));
                        false
                    }
                }
            }
            Err(_) => false,
        };
        let _ = CloseClipboard();
        ok
    }
}

fn on_select() {
    let Some(i) = selected() else { return };
    let Some(body) = bodies().get(i).cloned() else {
        return;
    };
    if let Some(edit) = UI.with(|u| u.borrow().as_ref().map(|d| d.edit)) {
        set_text(edit, &to_edit(&body));
    }
}

unsafe fn run(owner: HWND) {
    unsafe {
        let Ok(dlg) = create_dialog(owner) else {
            log("snippets: window failed to open");
            return;
        };
        let hwnd = dlg.hwnd;
        UI.with(|u| *u.borrow_mut() = Some(dlg));
        refresh(None);
        let _ = EnableWindow(owner, false);
        let _ = SetForegroundWindow(hwnd);
        let mut msg = MSG::default();
        while !DONE.get() {
            let ret = GetMessageW(&mut msg, None, 0, 0).0;
            if ret == 0 {
                PostQuitMessage(msg.wParam.0 as i32);
                break;
            }
            if ret < 0 {
                break;
            }
            if msg.message == WM_KEYDOWN
                && msg.wParam.0 == VK_ESCAPE.0 as usize
                && msg.lParam.0 & (1 << 30) == 0
            {
                let _ = DestroyWindow(hwnd);
                continue;
            }
            if IsDialogMessageW(hwnd, &msg).as_bool() {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        UI.with(|u| *u.borrow_mut() = None);
        let _ = EnableWindow(owner, true);
        let _ = SetForegroundWindow(owner);
    }
}

struct At {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

unsafe fn create_dialog(owner: HWND) -> windows::core::Result<Dialog> {
    unsafe {
        register();
        let dpi = GetDpiForWindow(owner).max(96);
        let px = |dip: i32| dip * dpi as i32 / 96;
        let (cw, ch) = (px(440), px(320));
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let ex = WS_EX_TOOLWINDOW | WS_EX_DLGMODALFRAME;
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: cw,
            bottom: ch,
        };
        let _ = AdjustWindowRectEx(&mut rc, style, false, ex);
        let (ww, hh) = (rc.right - rc.left, rc.bottom - rc.top);
        let origin = centered(owner, ww, hh);
        let hinst = GetModuleHandleW(None)?.into();
        let hwnd = CreateWindowExW(
            ex,
            w!("Tasca.Snippets"),
            w!("Snippets"),
            style | WS_VISIBLE,
            origin.0,
            origin.1,
            ww,
            hh,
            Some(owner),
            None,
            Some(hinst),
            None,
        )?;
        let font = GetStockObject(DEFAULT_GUI_FONT);
        sm(hwnd, WM_SETFONT, font.0 as usize, 1);
        let m = px(12);
        let gap = px(8);
        let btn_h = px(26);
        let btn_w = px(72);
        let list_h = px(120);
        let edit_y = m + list_h + gap;
        let edit_h = ch - edit_y - gap - btn_h - m;
        let list = control(
            hwnd,
            w!("LISTBOX"),
            w!(""),
            list_style(),
            ID_LIST,
            At {
                x: m,
                y: m,
                w: cw - 2 * m,
                h: list_h,
            },
        )?;
        let edit = control(
            hwnd,
            w!("EDIT"),
            w!(""),
            edit_style(),
            ID_EDIT,
            At {
                x: m,
                y: edit_y,
                w: cw - 2 * m,
                h: edit_h,
            },
        )?;
        let labels = [w!("New"), w!("Save"), w!("Delete"), w!("Copy"), w!("Close")];
        let ids = [ID_NEW, ID_SAVE, ID_DELETE, ID_COPY, ID_CLOSE];
        let y = ch - m - btn_h;
        for (i, (label, id)) in labels.iter().zip(ids).enumerate() {
            let style = WS_CHILD
                | WS_VISIBLE
                | WS_TABSTOP
                | if id == ID_SAVE {
                    WINDOW_STYLE(BS_DEFPUSHBUTTON as u32)
                } else {
                    WINDOW_STYLE(0)
                };
            let _ = control(
                hwnd,
                w!("BUTTON"),
                *label,
                style,
                id,
                At {
                    x: m + i as i32 * (btn_w + gap),
                    y,
                    w: btn_w,
                    h: btn_h,
                },
            )?;
        }
        sm(list, WM_SETFONT, font.0 as usize, 1);
        sm(edit, WM_SETFONT, font.0 as usize, 1);
        Ok(Dialog { hwnd, list, edit })
    }
}

fn list_style() -> WINDOW_STYLE {
    WS_CHILD
        | WS_VISIBLE
        | WS_TABSTOP
        | WS_VSCROLL
        | WS_BORDER
        | WINDOW_STYLE((LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32)
}

fn edit_style() -> WINDOW_STYLE {
    WS_CHILD
        | WS_VISIBLE
        | WS_TABSTOP
        | WS_VSCROLL
        | WS_BORDER
        | WINDOW_STYLE(ES_MULTILINE as u32)
        | WINDOW_STYLE(ES_AUTOVSCROLL as u32)
        | WINDOW_STYLE(ES_WANTRETURN as u32)
}

unsafe fn sm(hwnd: HWND, msg: u32, w: usize, l: isize) -> LRESULT {
    unsafe { SendMessageW(hwnd, msg, Some(WPARAM(w)), Some(LPARAM(l))) }
}

fn centered(owner: HWND, w: i32, h: i32) -> (i32, i32) {
    unsafe {
        let mut r = RECT::default();
        if GetWindowRect(owner, &mut r).is_ok() {
            let x = r.left + (r.right - r.left - w) / 2;
            let y = r.top + (r.bottom - r.top - h) / 2;
            return (x.max(0), y.max(0));
        }
    }
    (80, 80)
}

unsafe fn control(
    parent: HWND,
    class: PCWSTR,
    title: PCWSTR,
    style: WINDOW_STYLE,
    id: i32,
    at: At,
) -> windows::core::Result<HWND> {
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            title,
            style,
            at.x,
            at.y,
            at.w,
            at.h,
            Some(parent),
            Some(HMENU(id as isize as *mut _)),
            Some(GetModuleHandleW(None)?.into()),
            None,
        )
    }
}

fn register() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let hinst = GetModuleHandleW(None).map(|m| m.into()).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 as usize + 1) as *mut _),
            lpszClassName: w!("Tasca.Snippets"),
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);
    });
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_COMMAND => {
                let id = (wp.0 & 0xffff) as i32;
                let note = ((wp.0 >> 16) & 0xffff) as u32;
                match (id, note) {
                    (ID_LIST, LBN_SELCHANGE) => on_select(),
                    (ID_LIST, LBN_DBLCLK) => on_copy(),
                    (ID_NEW, 0) => on_new(),
                    (ID_SAVE, 0) => on_save(),
                    (ID_DELETE, 0) => on_delete(),
                    (ID_COPY, 0) => on_copy(),
                    (ID_CLOSE, 0) => {
                        let _ = DestroyWindow(hwnd);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                DONE.set(true);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_text_is_refused_and_trailing_space_goes() {
        assert_eq!(accept("  \n"), None);
        assert_eq!(accept("  hello \n").as_deref(), Some("  hello"));
    }

    #[test]
    fn the_same_text_moves_to_the_top() {
        let mut list = vec!["a".into(), "b".into()];
        remember(&mut list, "  b ".into());
        assert_eq!(list, vec!["  b ".to_string(), "a".to_string()]);
    }

    #[test]
    fn the_file_round_trips_and_a_bad_file_is_empty() {
        assert!(decode("nope").is_empty());
        assert!(decode("TSCS\n").is_empty());
        let items = vec!["one".to_string(), "two\nlines".to_string()];
        assert_eq!(decode(&encode(&items)), items);
        let dir = std::env::temp_dir().join(format!("tasca-snippets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("snippets.txt");
        assert!(load_from(&path).is_empty());
        write_file(&path, &items).unwrap();
        assert_eq!(load_from(&path), items);
        std::fs::write(&path, "garbage").unwrap();
        assert!(load_from(&path).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn newlines_round_trip_through_the_edit_control() {
        assert_eq!(from_edit(&to_edit("a\nb")), "a\nb");
    }
}
