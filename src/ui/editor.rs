use crate::{Context, Result, config::Layout, platform::wide};
use std::{
    cell::RefCell,
    ffi::{CString, c_void},
    ptr::{null, null_mut},
    sync::mpsc::{self, Receiver, SyncSender},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::*,
    UI::{Controls::EM_SETCUEBANNER, HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};
#[path = "scintilla.rs"]
mod sci;
use sci::*;
const TEXT: i32 = 100;
const SAVE: i32 = 101;
const FIND: i32 = 102;
const REPLACE: i32 = 103;
const NEXT: i32 = 104;
const REPLACE_ONE: i32 = 105;
const REPLACE_ALL: i32 = 106;
const GOTO: i32 = 107;
const GOTO_BUTTON: i32 = 108;
const WRAP: i32 = 109;
const STATUS: i32 = 110;
const UNDO: i32 = 111;
const REDO: i32 = 112;
const CASE: i32 = 113;
const HOSTS: i32 = 114;
pub(super) enum Event {
    Save(String, String, String),
    ExitReady,
    Error(String),
    HostsApply,
}
enum Request {
    Open {
        id: String,
        text: String,
        readonly: bool,
        path: Option<String>,
    },
    Saved(std::result::Result<String, String>),
    CheckExit,
    Shutdown,
}
pub(super) struct Client {
    tx: SyncSender<Request>,
}
impl Client {
    pub fn open(&self, id: &str, text: &str, readonly: bool, path: Option<&str>) -> Result<()> {
        self.tx
            .try_send(Request::Open {
                id: id.into(),
                text: text.into(),
                readonly,
                path: path.map(str::to_owned),
            })
            .context("编辑器忙碌或已关闭")
    }
    pub fn saved(&self, result: std::result::Result<String, String>) -> Result<()> {
        self.tx
            .try_send(Request::Saved(result))
            .context("编辑器未接收保存结果")
    }
    pub fn check_exit(&self) -> Result<()> {
        self.tx.try_send(Request::CheckExit).context("编辑器不可用")
    }
    pub fn shutdown(&self) -> Result<()> {
        self.tx.try_send(Request::Shutdown).context("编辑器不可用")
    }
}
struct Library(HMODULE);
impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: Editor windows and lexer objects are destroyed before library owners.
        unsafe {
            FreeLibrary(self.0);
        }
    }
}
fn library(layout: &Layout, name: &str) -> Result<Library> {
    let path = layout.path(format!("lib/{name}"))?;
    // SAFETY: Load only an explicit validated path, with dependencies restricted to its directory and System32.
    let handle = unsafe {
        LoadLibraryExW(
            wide(path.as_os_str()).as_ptr(),
            null_mut(),
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    crate::ensure!(
        !handle.is_null(),
        "无法加载 {}：{}",
        path.display(),
        std::io::Error::last_os_error()
    );
    Ok(Library(handle))
}
pub(super) fn start(layout: &Layout) -> Result<(Client, Receiver<Event>)> {
    let (tx, rx) = mpsc::sync_channel(8);
    let (events, output) = mpsc::sync_channel(8);
    let (ready, startup) = mpsc::sync_channel(1);
    let layout = layout.clone();
    std::thread::Builder::new()
        .name("config-editor".into())
        .spawn(move || {
            if let Err(e) = run(layout, rx, events.clone(), ready.clone()) {
                let text = format!("{e:#}");
                let _ = ready.try_send(Err(text.clone()));
                let _ = events.try_send(Event::Error(text));
            }
        })?;
    startup
        .recv()
        .context("编辑器线程未启动")?
        .map_err(anyhow::Error::msg)?;
    Ok((Client { tx }, output))
}
type CreateLexer = unsafe extern "C" fn(*const u8) -> *mut c_void;
struct Editor {
    hwnd: HWND,
    control: HWND,
    font: HFONT,
    rx: Receiver<Request>,
    events: SyncSender<Event>,
    id: String,
    title: String,
    baseline: String,
    readonly: bool,
    saving: bool,
    hint: String,
    create_lexer: CreateLexer,
}
impl Drop for Editor {
    fn drop(&mut self) {
        // SAFETY: Font is owned by this editor and all controls have been destroyed.
        unsafe {
            DeleteObject(self.font);
        }
    }
}
impl Editor {
    fn sci(&self, msg: u32, w: usize, l: isize) -> isize {
        // SAFETY: Calls stay on the window thread; callers supply synchronous valid pointer buffers where required.
        unsafe { SendMessageW(self.control, msg, w, l) }
    }
    fn text(&self) -> String {
        let len = self.sci(SCI_GETTEXTLENGTH, 0, 0).max(0) as usize;
        let mut buf = vec![0u8; len + 1];
        self.sci(SCI_GETTEXT, buf.len(), buf.as_mut_ptr() as isize);
        String::from_utf8_lossy(&buf[..len]).into_owned()
    }
    fn dirty(&self) -> bool {
        !self.readonly && self.sci(SCI_GETMODIFY, 0, 0) != 0
    }
    fn discard(&self) -> bool {
        if self.saving {
            return false;
        }
        if !self.dirty() {
            return true;
        }
        // SAFETY: The owner is live; buffers span this synchronous modal call.
        unsafe {
            MessageBoxW(
                self.hwnd,
                wide("有尚未应用的修改。放弃这些修改并继续？").as_ptr(),
                wide("Flaragon · 未保存的配置").as_ptr(),
                MB_YESNO | MB_DEFBUTTON2 | MB_ICONWARNING,
            ) == IDYES
        }
    }
    fn child(&self, id: i32) -> HWND {
        // SAFETY: The child belongs to this live editor window.
        unsafe { GetDlgItem(self.hwnd, id) }
    }
    fn set_hint(&mut self, text: impl Into<String>) {
        self.hint = text.into();
        self.status();
    }
    fn status(&self) {
        let pos = self.sci(SCI_GETCURRENTPOS, 0, 0) as usize;
        let line = self.sci(SCI_LINEFROMPOSITION, pos, 0) + 1;
        let col = self.sci(SCI_GETCOLUMN, pos, 0) + 1;
        let state = if self.saving {
            "正在检查并应用"
        } else if self.readonly {
            "只读"
        } else if self.dirty() {
            "未保存"
        } else {
            "没有未保存的修改"
        };
        // SAFETY: Text buffers span SetWindowText; the handles remain owned by this editor.
        unsafe {
            SetWindowTextW(
                self.child(STATUS),
                wide(format!(
                    "行 {line}，列 {col}  ·  共 {} 行  ·  UTF-8  ·  {state}    {}",
                    self.sci(SCI_GETLINECOUNT, 0, 0),
                    self.hint
                ))
                .as_ptr(),
            );
            SetWindowTextW(
                self.hwnd,
                wide(format!(
                    "{}{} — Flaragon 配置编辑器",
                    if self.dirty() { "* " } else { "" },
                    self.title
                ))
                .as_ptr(),
            );
            EnableWindow(self.child(SAVE), (!self.readonly && !self.saving) as i32);
            for id in [REPLACE_ONE, REPLACE_ALL, UNDO, REDO] {
                EnableWindow(self.child(id), (!self.readonly && !self.saving) as i32);
            }
        }
    }
    fn load(
        &mut self,
        id: String,
        text: String,
        readonly: bool,
        path: Option<String>,
    ) -> Result<()> {
        if !self.discard() {
            return Ok(());
        }
        let content = CString::new(text.clone()).context("配置含有 NUL 字节，无法编辑")?;
        self.baseline = text;
        self.id = id;
        self.title = path.unwrap_or_else(|| self.id.clone());
        self.readonly = readonly;
        let lexer = if self.id.starts_with("php:") || self.id.starts_with("mysql:") {
            c"props"
        } else {
            c"conf"
        };
        // SAFETY: Function was resolved from the bundled Lexilla module and uses its C ABI. Scintilla owns the returned lexer.
        let lexer_pointer = unsafe { (self.create_lexer)(lexer.as_ptr().cast()) };
        crate::ensure!(!lexer_pointer.is_null(), "语法高亮器不可用");
        self.sci(SCI_SETILEXER, 0, lexer_pointer as isize);
        self.sci(SCI_SETREADONLY, 0, 0);
        self.sci(SCI_SETTEXT, 0, content.as_ptr() as isize);
        self.sci(SCI_EMPTYUNDOBUFFER, 0, 0);
        self.sci(SCI_SETSAVEPOINT, 0, 0);
        self.sci(SCI_SETREADONLY, readonly as usize, 0);
        self.sci(SCI_COLOURISE, 0, -1);
        self.hint = if readonly {
            String::new()
        } else {
            "Ctrl+S 检查并应用 · Ctrl+F 查找 · Ctrl+H 替换 · Ctrl+G 跳转".into()
        };
        // SAFETY: Shows this window and focuses its live Scintilla child.
        unsafe {
            ShowWindow(
                self.child(HOSTS),
                if self.id == "hosts" { SW_SHOW } else { SW_HIDE },
            );
            ShowWindow(self.hwnd, SW_RESTORE);
            SetForegroundWindow(self.hwnd);
            SetFocus(self.control);
        }
        self.status();
        Ok(())
    }
    fn save(&mut self) {
        if self.readonly || self.saving {
            return;
        }
        match self.events.try_send(Event::Save(
            self.id.clone(),
            self.text(),
            self.baseline.clone(),
        )) {
            Ok(()) => {
                self.saving = true;
                self.sci(SCI_SETREADONLY, 1, 0);
                self.set_hint("保存成功后显示实际应用结果");
            }
            Err(_) => self.set_hint("任务队列忙，请稍后重试"),
        }
    }
    fn saved(&mut self, result: std::result::Result<String, String>) {
        self.saving = false;
        self.sci(SCI_SETREADONLY, self.readonly as usize, 0);
        match result {
            Ok(text) => {
                self.baseline = text.clone();
                if text != self.text() {
                    let pos = self.sci(SCI_GETCURRENTPOS, 0, 0);
                    if let Ok(content) = CString::new(text) {
                        self.sci(SCI_BEGINUNDOACTION, 0, 0);
                        self.sci(SCI_SETTARGETSTART, 0, 0);
                        self.sci(
                            SCI_SETTARGETEND,
                            self.sci(SCI_GETTEXTLENGTH, 0, 0) as usize,
                            0,
                        );
                        self.sci(
                            SCI_REPLACETARGET,
                            content.as_bytes().len(),
                            content.as_ptr() as isize,
                        );
                        self.sci(SCI_ENDUNDOACTION, 0, 0);
                        self.sci(SCI_SETSEL, pos as usize, pos);
                    }
                }
                self.sci(SCI_SETSAVEPOINT, 0, 0);
                self.set_hint("配置已保存；运行服务已完成应用验证，停止的服务将在启动时验证");
            }
            Err(error) => {
                self.set_hint("应用失败，编辑内容已保留"); // SAFETY: Error text stays valid during the owned modal dialog.
                unsafe {
                    MessageBoxW(
                        self.hwnd,
                        wide(&error).as_ptr(),
                        wide("配置未应用").as_ptr(),
                        MB_OK | MB_ICONERROR,
                    );
                }
            }
        }
    }
    fn find(&mut self) -> bool {
        let query = get_text(self.child(FIND));
        if query.is_empty() {
            self.set_hint("请输入查找内容");
            return false;
        }
        let start = self.sci(SCI_GETSELECTIONEND, 0, 0);
        let len = self.sci(SCI_GETTEXTLENGTH, 0, 0);
        self.search_flags();
        for (a, b) in [(start, len), (0, start)] {
            self.sci(SCI_SETTARGETSTART, a as usize, 0);
            self.sci(SCI_SETTARGETEND, b as usize, 0);
            let found = self.sci(SCI_SEARCHINTARGET, query.len(), query.as_ptr() as isize);
            if found >= 0 {
                let end = self.sci(SCI_GETTARGETEND, 0, 0);
                self.sci(SCI_SETSEL, found as usize, end);
                self.sci(SCI_SCROLLCARET, 0, 0);
                self.set_hint("已找到匹配项");
                return true;
            }
        }
        self.set_hint("没有找到匹配项");
        false
    }
    fn search_flags(&self) {
        // SAFETY: The checkbox is an owned live child.
        let flags = unsafe {
            if SendMessageW(self.child(CASE), BM_GETCHECK, 0, 0) == 1 {
                4
            } else {
                0
            }
        };
        self.sci(SCI_SETSEARCHFLAGS, flags, 0);
    }
    fn replace(&mut self, all: bool) {
        if self.readonly || self.saving {
            return;
        }
        let query = get_text(self.child(FIND));
        let replacement = get_text(self.child(REPLACE));
        if query.is_empty() {
            self.set_hint("请输入查找内容");
            return;
        }
        self.search_flags();
        self.sci(SCI_BEGINUNDOACTION, 0, 0);
        let mut count = 0;
        if all {
            let mut start = 0;
            loop {
                self.sci(SCI_SETTARGETSTART, start, 0);
                self.sci(
                    SCI_SETTARGETEND,
                    self.sci(SCI_GETTEXTLENGTH, 0, 0) as usize,
                    0,
                );
                if self.sci(SCI_SEARCHINTARGET, query.len(), query.as_ptr() as isize) < 0 {
                    break;
                }
                self.sci(
                    SCI_REPLACETARGET,
                    replacement.len(),
                    replacement.as_ptr() as isize,
                );
                start = self.sci(SCI_GETTARGETEND, 0, 0) as usize;
                count += 1;
            }
        } else {
            let start = self.sci(SCI_GETSELECTIONSTART, 0, 0);
            let end = self.sci(SCI_GETSELECTIONEND, 0, 0);
            self.sci(SCI_SETTARGETSTART, start as usize, 0);
            self.sci(SCI_SETTARGETEND, end as usize, 0);
            if self.sci(SCI_SEARCHINTARGET, query.len(), query.as_ptr() as isize) == start
                && self.sci(SCI_GETTARGETEND, 0, 0) == end
            {
                self.sci(
                    SCI_REPLACETARGET,
                    replacement.len(),
                    replacement.as_ptr() as isize,
                );
                self.sci(
                    SCI_SETSEL,
                    start as usize,
                    start + replacement.len() as isize,
                );
                count = 1;
            }
        }
        self.sci(SCI_ENDUNDOACTION, 0, 0);
        if !all {
            self.find();
        }
        self.set_hint(format!("已替换 {count} 项；Ctrl+Z 可撤销"));
    }
    fn command(&mut self, id: i32) {
        match id {
            SAVE => self.save(),
            NEXT => {
                self.find();
            }
            REPLACE_ONE => self.replace(false),
            REPLACE_ALL => self.replace(true),
            UNDO => {
                self.sci(SCI_UNDO, 0, 0);
            }
            REDO => {
                self.sci(SCI_REDO, 0, 0);
            }
            WRAP => {
                self.sci(
                    SCI_SETWRAPMODE,
                    (self.sci(SCI_GETWRAPMODE, 0, 0) == 0) as usize,
                    0,
                );
            }
            GOTO_BUTTON => {
                if let Ok(line) = get_text(self.child(GOTO)).parse::<usize>() {
                    if line > 0 && line <= self.sci(SCI_GETLINECOUNT, 0, 0) as usize {
                        self.sci(SCI_GOTOLINE, line - 1, 0);
                        unsafe {
                            SetFocus(self.control);
                        }
                    } else {
                        self.set_hint("行号超出范围");
                    }
                } else {
                    self.set_hint("请输入有效行号");
                }
            }
            HOSTS => {
                // SAFETY: The preview remains visible and the user confirms the explicit system operation.
                if unsafe {
                    MessageBoxW(
                        self.hwnd,
                        wide("将当前环境的域名映射应用到系统 hosts，需要管理员授权。继续？")
                            .as_ptr(),
                        wide("应用域名映射").as_ptr(),
                        MB_YESNO | MB_DEFBUTTON2,
                    )
                } == IDYES
                {
                    let _ = self.events.try_send(Event::HostsApply);
                }
            }
            _ => (),
        }
        self.status();
    }
    fn tick(&mut self) {
        for _ in 0..8 {
            let Ok(request) = self.rx.try_recv() else {
                break;
            };
            match request {
                Request::Open {
                    id,
                    text,
                    readonly,
                    path,
                } => {
                    if let Err(e) = self.load(id, text, readonly, path) {
                        let _ = self.events.try_send(Event::Error(format!("{e:#}")));
                    }
                }
                Request::Saved(result) => self.saved(result),
                Request::CheckExit => {
                    if self.discard() {
                        self.sci(SCI_SETSAVEPOINT, 0, 0);
                        unsafe {
                            ShowWindow(self.hwnd, SW_HIDE);
                        }
                        let _ = self.events.try_send(Event::ExitReady);
                    }
                }
                Request::Shutdown => {
                    unsafe {
                        DestroyWindow(self.hwnd);
                    }
                    return;
                }
            }
        }
        self.status();
    }
}
fn get_text(hwnd: HWND) -> String {
    // SAFETY: Synchronous read allocates room for the UTF-16 terminator.
    unsafe {
        let len = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut text = vec![0u16; len + 1];
        GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32);
        String::from_utf16_lossy(&text[..len])
    }
}
fn run(
    layout: Layout,
    rx: Receiver<Request>,
    events: SyncSender<Event>,
    ready: SyncSender<std::result::Result<(), String>>,
) -> Result<()> {
    let _scintilla = library(&layout, "Scintilla.dll")?;
    let lexilla = library(&layout, "Lexilla.dll")?;
    // SAFETY: Libraries remain loaded through the window loop. All handles and callbacks belong to this thread.
    unsafe {
        let create = GetProcAddress(lexilla.0, c"CreateLexer".as_ptr().cast())
            .context("Lexilla 缺少 CreateLexer")?;
        let create_lexer: CreateLexer = std::mem::transmute(create);
        let instance = GetModuleHandleW(null());
        let class = wide("FlaragonScintillaEditor");
        let wc = WNDCLASSW {
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            lpfnWndProc: Some(window_proc),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
            hIcon: LoadIconW(instance, 101usize as *const u16),
            ..std::mem::zeroed()
        };
        crate::ensure!(RegisterClassW(&wc) != 0, "编辑器窗口注册失败");
        let state = Box::new(RefCell::new(Editor {
            hwnd: null_mut(),
            control: null_mut(),
            font: null_mut(),
            rx,
            events,
            id: String::new(),
            title: "配置".into(),
            baseline: String::new(),
            readonly: true,
            saving: false,
            hint: String::new(),
            create_lexer,
        }));
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("Flaragon 配置编辑器").as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1140,
            780,
            null_mut(),
            null_mut(),
            instance,
            (&*state as *const RefCell<Editor>).cast(),
        );
        crate::ensure!(!hwnd.is_null(), "编辑器窗口创建失败");
        {
            let mut e = state.borrow_mut();
            e.hwnd = hwnd;
            let dpi = GetDpiForWindow(hwnd).max(96) as i32;
            e.font = CreateFontW(
                -13 * dpi / 96,
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                1,
                0,
                0,
                5,
                0,
                wide("Microsoft YaHei UI").as_ptr(),
            );
            for (id, class, label, style) in [
                (TEXT, "Scintilla", "", WS_TABSTOP | WS_VSCROLL | WS_HSCROLL),
                (SAVE, "BUTTON", "检查并应用 Ctrl+S", WS_TABSTOP),
                (UNDO, "BUTTON", "撤销", WS_TABSTOP),
                (REDO, "BUTTON", "重做", WS_TABSTOP),
                (WRAP, "BUTTON", "自动换行", WS_TABSTOP),
                (FIND, "EDIT", "", WS_TABSTOP | ES_AUTOHSCROLL as u32),
                (NEXT, "BUTTON", "查找下一个", WS_TABSTOP),
                (
                    CASE,
                    "BUTTON",
                    "区分大小写",
                    WS_TABSTOP | BS_AUTOCHECKBOX as u32,
                ),
                (REPLACE, "EDIT", "", WS_TABSTOP | ES_AUTOHSCROLL as u32),
                (REPLACE_ONE, "BUTTON", "替换", WS_TABSTOP),
                (REPLACE_ALL, "BUTTON", "全部替换", WS_TABSTOP),
                (GOTO, "EDIT", "", WS_TABSTOP | ES_NUMBER as u32),
                (GOTO_BUTTON, "BUTTON", "跳转行", WS_TABSTOP),
                (STATUS, "STATIC", "", 0),
                (HOSTS, "BUTTON", "应用域名映射", WS_TABSTOP),
            ] {
                let h = CreateWindowExW(
                    if class == "EDIT" { WS_EX_CLIENTEDGE } else { 0 },
                    wide(class).as_ptr(),
                    wide(label).as_ptr(),
                    WS_CHILD | WS_VISIBLE | style,
                    0,
                    0,
                    10,
                    10,
                    hwnd,
                    id as HMENU,
                    instance,
                    null(),
                );
                crate::ensure!(!h.is_null(), "编辑器控件创建失败：{class}");
                SendMessageW(h, WM_SETFONT, e.font as usize, 1);
                if id == TEXT {
                    e.control = h;
                }
            }
            e.sci(SCI_SETCODEPAGE, 65001, 0);
            e.sci(SCI_STYLESETFONT, 32, c"Consolas".as_ptr() as isize);
            e.sci(SCI_STYLESETSIZE, 32, 11);
            e.sci(SCI_STYLESETFORE, 32, 0x443525);
            e.sci(SCI_STYLESETBACK, 32, 0xffffff);
            e.sci(SCI_STYLECLEARALL, 0, 0);
            for (style, color) in [
                (1, 0x83958a),
                (2, 0xae5f85),
                (3, 0xa55535),
                (4, 0xb15a49),
                (5, 0x53833e),
                (6, 0xc45b57),
                (7, 0x986b49),
                (8, 0xad613a),
                (9, 0x9b7292),
                (33, 0x9b8c7a),
            ] {
                e.sci(SCI_STYLESETFORE, style, color);
            }
            e.sci(SCI_STYLESETBACK, 33, 0xf9f6f3);
            e.sci(SCI_SETMARGINTYPEN, 0, 1);
            e.sci(SCI_SETMARGINWIDTHN, 0, (58 * dpi / 96) as isize);
            e.sci(SCI_SETMARGINWIDTHN, 1, 0);
            e.sci(SCI_SETCARETLINEVISIBLE, 1, 0);
            e.sci(SCI_SETCARETLINEBACK, 0xfff8f2, 0);
            e.sci(SCI_SETCARETFORE, 0x885533, 0);
            e.sci(SCI_SETSELBACK, 1, 0xf5d9c7);
            e.sci(SCI_SETSELFORE, 0, 0);
            e.sci(SCI_SETTABWIDTH, 4, 0);
            e.sci(SCI_SETUSETABS, 0, 0);
            e.sci(SCI_SETEOLMODE, 0, 0);
            e.sci(SCI_SETREADONLY, 1, 0);
            ShowWindow(e.child(HOSTS), SW_HIDE);
            SendMessageW(
                e.child(FIND),
                EM_SETCUEBANNER,
                1,
                wide("查找 Ctrl+F").as_ptr() as isize,
            );
            SendMessageW(
                e.child(REPLACE),
                EM_SETCUEBANNER,
                1,
                wide("替换为 Ctrl+H").as_ptr() as isize,
            );
        }
        resize(hwnd);
        SetTimer(hwnd, 1, 150, None);
        let _ = ready.send(Ok(()));
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if msg.message == WM_KEYDOWN {
                let ctrl = GetKeyState(VK_CONTROL as i32) < 0;
                if ctrl && matches!(msg.wParam, 0x53 | 0x46 | 0x48 | 0x47) {
                    if let Ok(mut e) = state.try_borrow_mut() {
                        match msg.wParam {
                            0x53 => e.save(),
                            0x46 => {
                                SetFocus(e.child(FIND));
                            }
                            0x48 => {
                                SetFocus(e.child(REPLACE));
                            }
                            _ => {
                                SetFocus(e.child(GOTO));
                            }
                        }
                    }
                    continue;
                }
                if msg.wParam == VK_RETURN as usize {
                    let focus = GetFocus();
                    let e = state.borrow();
                    let command = if focus == e.child(FIND) {
                        Some(NEXT)
                    } else if focus == e.child(GOTO) {
                        Some(GOTO_BUTTON)
                    } else {
                        None
                    };
                    drop(e);
                    if let Some(command) = command {
                        state.borrow_mut().command(command);
                        continue;
                    }
                }
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        UnregisterClassW(class.as_ptr(), instance);
    }
    Ok(())
}
fn resize(hwnd: HWND) {
    // SAFETY: All child handles belong to the same live editor, and dimensions are clamped.
    unsafe {
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        let px = |v| v * dpi / 96;
        let mut rect: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut rect);
        for (id, x, y, w, h) in [
            (SAVE, 14, 12, 165, 32),
            (UNDO, 190, 12, 60, 32),
            (REDO, 258, 12, 60, 32),
            (WRAP, 330, 12, 90, 32),
            (HOSTS, 438, 12, 140, 32),
            (FIND, 14, 55, 250, 28),
            (NEXT, 274, 55, 102, 28),
            (CASE, 389, 55, 120, 28),
            (REPLACE, 14, 92, 250, 28),
            (REPLACE_ONE, 274, 92, 75, 28),
            (REPLACE_ALL, 359, 92, 100, 28),
            (GOTO, 480, 92, 80, 28),
            (GOTO_BUTTON, 570, 92, 75, 28),
        ] {
            MoveWindow(GetDlgItem(hwnd, id), px(x), px(y), px(w), px(h), 1);
        }
        MoveWindow(
            GetDlgItem(hwnd, TEXT),
            px(14),
            px(134),
            (rect.right - px(28)).max(1),
            (rect.bottom - px(170)).max(1),
            1,
        );
        MoveWindow(
            GetDlgItem(hwnd, STATUS),
            px(14),
            (rect.bottom - px(28)).max(0),
            (rect.right - px(28)).max(1),
            px(24),
            1,
        );
    }
}
unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    std::panic::catch_unwind(|| message(hwnd, msg, w, l)).unwrap_or(0)
}
fn message(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: State is boxed for the full message loop; borrow checks prevent nested synchronous messages from aliasing mutations.
    unsafe {
        if msg == WM_NCCREATE {
            let create = &*(l as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        if msg == WM_SIZE {
            resize(hwnd);
            return 0;
        }
        if msg == WM_DPICHANGED {
            let r = &*(l as *const RECT);
            SetWindowPos(
                hwnd,
                null_mut(),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER,
            );
            resize(hwnd);
            return 0;
        }
        if msg == WM_GETMINMAXINFO {
            let info = &mut *(l as *mut MINMAXINFO);
            let dpi = GetDpiForWindow(hwnd).max(96) as i32;
            info.ptMinTrackSize = POINT {
                x: 760 * dpi / 96,
                y: 480 * dpi / 96,
            };
            return 0;
        }
        if msg == WM_DESTROY {
            KillTimer(hwnd, 1);
            PostQuitMessage(0);
            return 0;
        }
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<Editor>;
        if !pointer.is_null()
            && let Ok(mut e) = (*pointer).try_borrow_mut()
        {
            match msg {
                WM_TIMER => {
                    e.tick();
                    return 0;
                }
                WM_COMMAND => {
                    if (w >> 16) == 0 {
                        e.command((w & 0xffff) as i32);
                    }
                    return 0;
                }
                WM_CLOSE => {
                    if e.discard() {
                        e.sci(SCI_SETSAVEPOINT, 0, 0);
                        ShowWindow(hwnd, SW_HIDE);
                    }
                    return 0;
                }
                _ => (),
            }
        }
        DefWindowProcW(hwnd, msg, w, l)
    }
}
