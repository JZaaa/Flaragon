use crate::{Context, Result, ensure};
use std::{
    ffi::OsStr,
    fs,
    io::Read,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*, NetworkManagement::IpHelper::*, Networking::WinSock::AF_INET,
    Storage::FileSystem::*, System::Threading::*,
};

pub mod deployment;
pub(crate) mod startup;

pub fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

/// Runs on the worker thread; the UI keeps its owner window alive until completion.
pub fn choose_directory(
    layout: &crate::config::Layout,
    owner: isize,
    initial: &Path,
) -> Result<Option<PathBuf>> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{
        System::Com::*,
        UI::{Shell::*, WindowsAndMessaging::SendMessageW},
    };
    struct ComApartment;
    impl Drop for ComApartment {
        fn drop(&mut self) {
            // SAFETY: This guard follows a successful CoInitializeEx on this thread.
            unsafe {
                CoUninitialize();
            }
        }
    }
    struct ItemList(*mut Common::ITEMIDLIST);
    impl Drop for ItemList {
        fn drop(&mut self) {
            // SAFETY: Shell-allocated PIDLs use the COM task allocator; null is allowed.
            unsafe {
                CoTaskMemFree(self.0.cast());
            }
        }
    }
    unsafe extern "system" fn initialized(
        window: HWND,
        message: u32,
        _: LPARAM,
        data: LPARAM,
    ) -> i32 {
        if message == BFFM_INITIALIZED {
            // SAFETY: data points to the initial UTF-16 path kept alive throughout the dialog.
            unsafe {
                SendMessageW(window, BFFM_SETSELECTIONW, 1, data);
            }
        }
        0
    }
    let initial = layout
        .site_path(initial)
        .ok()
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| layout.root.clone());
    let initial = wide(initial);
    let title = wide("选择网站目录");
    // SAFETY: COM and PIDLs are owned by the guards; all buffers outlive the modal call.
    // The owner belongs to the live UI, which awaits the worker event without blocking.
    unsafe {
        let status = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        ensure!(status >= 0, "无法初始化目录选择器：{status:#x}");
        let _apartment = ComApartment;
        let mut root = ItemList(std::ptr::null_mut());
        if let Some(boundary) = layout.development_root() {
            crate::config::reject_links(boundary)?;
            let status = SHParseDisplayName(
                wide(boundary).as_ptr(),
                std::ptr::null_mut(),
                &mut root.0,
                0,
                std::ptr::null_mut(),
            );
            ensure!(status >= 0, "无法打开开发隔离目录：{status:#x}");
        }
        let mut display = [0u16; 260];
        let info = BROWSEINFOW {
            hwndOwner: owner as HWND,
            pidlRoot: root.0,
            pszDisplayName: display.as_mut_ptr(),
            lpszTitle: title.as_ptr(),
            ulFlags: BIF_RETURNONLYFSDIRS
                | BIF_NEWDIALOGSTYLE
                | BIF_EDITBOX
                | BIF_NONEWFOLDERBUTTON,
            lpfn: Some(initialized),
            lParam: initial.as_ptr() as LPARAM,
            iImage: 0,
        };
        let selected = ItemList(SHBrowseForFolderW(&info));
        if selected.0.is_null() {
            return Ok(None);
        }
        let mut path = vec![0u16; 32768];
        ensure!(
            SHGetPathFromIDListEx(
                selected.0,
                path.as_mut_ptr(),
                path.len() as u32,
                GPFIDL_DEFAULT
            ) != 0,
            "请选择本机文件夹"
        );
        let length = path.iter().position(|c| *c == 0).context("目录路径无效")?;
        let path = layout.site_path(PathBuf::from(std::ffi::OsString::from_wide(
            &path[..length],
        )))?;
        ensure!(path.is_dir(), "目录不存在：{}", path.display());
        Ok(Some(path))
    }
}
pub fn replace_file(from: &Path, to: &Path) -> Result<()> {
    let from = wide(from);
    let to = wide(to);
    // SAFETY: Both buffers are terminated UTF-16 and live through the synchronous call.
    ensure!(
        unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } != 0,
        "{}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

pub fn publish_new_file(from: &Path, to: &Path) -> Result<()> {
    let from = wide(from);
    let to = wide(to);
    // SAFETY: Both buffers remain valid during the call; omitting REPLACE_EXISTING preserves concurrent edits.
    ensure!(
        unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } != 0,
        "无法提交新文件（目标可能已存在）：{}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

pub fn command(exe: &Path, root: &Path, cwd: &Path) -> Command {
    let mut cmd = Command::new(exe);
    cmd.creation_flags(CREATE_NO_WINDOW)
        .current_dir(cwd)
        .stdin(Stdio::null());
    let system = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    cmd.env_clear()
        .env("SystemRoot", &system)
        .env("WINDIR", &system);
    let paths = [
        exe.parent().unwrap_or(root).to_path_buf(),
        std::path::PathBuf::from(&system).join("System32"),
        std::path::PathBuf::from(&system),
    ];
    if let Ok(path) = std::env::join_paths(paths) {
        cmd.env("PATH", path);
    }
    let tmp = root.join("runtime/temp");
    cmd.env("TEMP", &tmp)
        .env("TMP", &tmp)
        .env("HOME", root)
        .env("USERPROFILE", root)
        .env("APPDATA", root.join("runtime"))
        .env("LOCALAPPDATA", root.join("runtime"));
    cmd
}
pub fn capture(cmd: &mut Command, timeout: Duration) -> Result<String> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .context("无法启动组件（检查 x64 运行库与路径）")?;
    let out = child.stdout.take().context("缺少 stdout")?;
    let err = child.stderr.take().context("缺少 stderr")?;
    fn drain(mut stream: impl Read) -> Vec<u8> {
        let mut kept = Vec::new();
        let mut buf = [0; 4096];
        while let Ok(n) = stream.read(&mut buf) {
            if n == 0 {
                break;
            }
            let take = n.min(131072usize.saturating_sub(kept.len()));
            kept.extend_from_slice(&buf[..take]);
        }
        kept
    }
    let a = thread::spawn(move || drain(out));
    let b = thread::spawn(move || drain(err));
    let start = Instant::now();
    let status = loop {
        if let Some(code) = child.try_wait()? {
            break code;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            crate::bail!("组件命令执行超时");
        }
        thread::sleep(Duration::from_millis(40));
    };
    let mut text = String::from_utf8_lossy(&a.join().unwrap_or_default()).to_string();
    text.push_str(&String::from_utf8_lossy(&b.join().unwrap_or_default()));
    ensure!(status.success(), "组件命令失败 ({status})：{}", text.trim());
    Ok(text)
}

pub fn spawn_logged(cmd: &mut Command, log: &Path) -> Result<Child> {
    let file = fs::OpenOptions::new().create(true).append(true).open(log)?;
    cmd.stdout(file.try_clone()?)
        .stderr(file)
        .spawn()
        .context("组件进程启动失败")
}
pub fn wait_exit(child: &mut Child, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        ensure!(start.elapsed() < timeout, "正常关闭超时，进程仍保留运行");
        thread::sleep(Duration::from_millis(80));
    }
}

pub fn local_ipv4_addresses() -> Result<Vec<String>> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{GetAdaptersInfo, IP_ADAPTER_INFO};
    let mut size = 0;
    // SAFETY: The first call only retrieves the required size. Subsequent pointers are
    // supplied by Windows within the live, aligned buffer and used before it is freed.
    unsafe {
        let status = GetAdaptersInfo(std::ptr::null_mut(), &mut size);
        if status == 232 {
            return Ok(vec!["127.0.0.1".into()]);
        }
        ensure!(status == 111 || status == 0, "读取网卡失败：{status}");
        for _ in 0..3 {
            ensure!(size <= 1024 * 1024, "网卡信息超出上限");
            let mut buffer = vec![0u64; (size as usize).div_ceil(8).max(1)];
            let status = GetAdaptersInfo(buffer.as_mut_ptr().cast(), &mut size);
            if status == 111 {
                continue;
            }
            ensure!(status == 0, "读取网卡失败：{status}");
            let mut result = std::collections::BTreeSet::from(["127.0.0.1".to_string()]);
            let mut adapter = buffer.as_ptr().cast::<IP_ADAPTER_INFO>();
            while !adapter.is_null() {
                let mut address = &(*adapter).IpAddressList
                    as *const windows_sys::Win32::NetworkManagement::IpHelper::IP_ADDR_STRING;
                while !address.is_null() {
                    let bytes: Vec<u8> = (*address)
                        .IpAddress
                        .String
                        .iter()
                        .take_while(|c| **c != 0)
                        .map(|c| *c as u8)
                        .collect();
                    if let Ok(ip) = String::from_utf8(bytes)
                        && ip
                            .parse::<std::net::Ipv4Addr>()
                            .is_ok_and(|ip| !ip.is_unspecified())
                    {
                        result.insert(ip);
                    }
                    address = (*address).Next;
                }
                adapter = (*adapter).Next;
            }
            return Ok(result.into_iter().collect());
        }
    }
    crate::bail!("网卡地址正在变化，请刷新组件后重试")
}

pub fn copy_text(text: &str) -> Result<()> {
    use windows_sys::Win32::System::{DataExchange::*, Memory::*};
    // SAFETY: The UTF-16 source is live; GlobalAlloc owns sufficient bytes. After a
    // successful SetClipboardData Windows owns the allocation; failures free it.
    unsafe {
        ensure!(
            OpenClipboard(windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow()) != 0,
            "剪贴板正在被其他程序使用"
        );
        let result = (|| -> Result<()> {
            let value = wide(text);
            let memory = GlobalAlloc(GMEM_MOVEABLE, value.len() * 2);
            ensure!(!memory.is_null(), "无法分配剪贴板内存");
            let destination = GlobalLock(memory).cast::<u16>();
            if destination.is_null() {
                windows_sys::Win32::Foundation::GlobalFree(memory);
                crate::bail!("无法锁定剪贴板内存");
            }
            std::ptr::copy_nonoverlapping(value.as_ptr(), destination, value.len());
            GlobalUnlock(memory);
            if EmptyClipboard() == 0 || SetClipboardData(13, memory).is_null() {
                windows_sys::Win32::Foundation::GlobalFree(memory);
                crate::bail!("无法复制地址");
            }
            Ok(())
        })();
        CloseClipboard();
        result
    }
}

pub fn listening_pid(port: u16) -> Result<Option<u32>> {
    // SAFETY: Initial call requests size. u64 storage ensures alignment for TCP table rows.
    unsafe {
        let mut bytes = 0;
        let code = GetExtendedTcpTable(
            std::ptr::null_mut(),
            &mut bytes,
            0,
            AF_INET as u32,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        );
        ensure!(
            code == ERROR_INSUFFICIENT_BUFFER || code == 0,
            "读取端口归属失败：{code}"
        );
        for _ in 0..3 {
            let mut storage = vec![0u64; (bytes as usize).div_ceil(8)];
            let code = GetExtendedTcpTable(
                storage.as_mut_ptr().cast(),
                &mut bytes,
                0,
                AF_INET as u32,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            );
            if code == ERROR_INSUFFICIENT_BUFFER {
                continue;
            }
            ensure!(code == 0, "读取端口归属失败：{code}");
            let base = storage.as_ptr().cast::<u8>();
            let count = std::ptr::read_unaligned(base.cast::<u32>()) as usize;
            let row_size = std::mem::size_of::<MIB_TCPROW_OWNER_PID>();
            ensure!(4 + count * row_size <= storage.len() * 8, "TCP 表长度异常");
            for i in 0..count {
                let row = std::ptr::read_unaligned(
                    base.add(4 + i * row_size).cast::<MIB_TCPROW_OWNER_PID>(),
                );
                if u16::from_be(row.dwLocalPort as u16) == port {
                    return Ok(Some(row.dwOwningPid));
                }
            }
            return Ok(None);
        }
    }
    crate::bail!("端口归属发生变化，请重试")
}

pub fn mysql_shutdown(pid: u32) -> Result<()> {
    // SAFETY: The documented MySQL shutdown event belongs to a retained live child PID.
    unsafe {
        let mut handle = OpenEventW(
            EVENT_MODIFY_STATE,
            0,
            wide(format!("mysqld{pid}_shutdown")).as_ptr(),
        );
        if handle.is_null() {
            handle = OpenEventW(
                EVENT_MODIFY_STATE,
                0,
                wide(format!("MYSQLShutdown{pid}")).as_ptr(),
            );
        }
        if handle.is_null() {
            handle = OpenEventW(
                EVENT_MODIFY_STATE,
                0,
                wide(format!("MySQLShutdown{pid}")).as_ptr(),
            );
        }
        ensure!(
            !handle.is_null(),
            "无法打开 MySQL 关闭事件：{}",
            std::io::Error::last_os_error()
        );
        let ok = SetEvent(handle);
        CloseHandle(handle);
        ensure!(ok != 0, "MySQL 正常关闭请求失败");
    }
    Ok(())
}

pub fn apache_shutdown(pid: u32) -> Result<()> {
    let name = wide(format!("ap{pid}_shutdown"));
    // SAFETY: Caller holds the identified Apache parent process handle. The winnt MPM names its graceful shutdown event by parent PID.
    unsafe {
        let handle = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
        ensure!(
            !handle.is_null(),
            "无法打开 Apache 正常关闭事件：{}",
            std::io::Error::last_os_error()
        );
        let ok = SetEvent(handle);
        CloseHandle(handle);
        ensure!(ok != 0, "Apache 正常关闭请求失败");
    }
    Ok(())
}

pub fn apache_reload(process: &Process) -> Result<()> {
    ensure!(process.running(), "Apache 主进程已退出");
    let name = wide(format!("ap{}_restart", process.identity.pid));
    // SAFETY: The caller retains the identified parent process; WinNT MPM owns this PID-named event.
    unsafe {
        let handle = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
        ensure!(
            !handle.is_null(),
            "无法打开 Apache 重加载事件：{}",
            std::io::Error::last_os_error()
        );
        let ok = SetEvent(handle);
        CloseHandle(handle);
        ensure!(ok != 0, "Apache 重加载请求失败");
    }
    Ok(())
}

pub struct AppLock {
    handle: HANDLE,
    _gate: std::fs::File,
}
// SAFETY: The mutex handle is owned, never exposed, and only closed on drop.
unsafe impl Send for AppLock {}
impl AppLock {
    pub fn acquire(root: &Path) -> Result<Self> {
        let gate = crate::deployment::lock_directory(root)?;
        crate::deployment::ensure_ready(root)?;
        let name = wide(format!(
            "Local\\Flaragon_{}",
            root.to_string_lossy()
                .to_lowercase()
                .replace(['\\', '/', ':'], "_")
        ));
        // SAFETY: Valid terminated string; default security descriptor; owned handle checked below.
        unsafe {
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            ensure!(!handle.is_null(), "无法建立实例锁");
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(handle);
                crate::bail!("此环境已由另一个 Flaragon 进程管理，请使用已有窗口");
            }
            Ok(Self {
                handle,
                _gate: gate,
            })
        }
    }
}

pub struct RestoreSignal(OwnedHandle);
impl RestoreSignal {
    fn name(root: &Path) -> Vec<u16> {
        wide(format!(
            "Local\\FlaragonRestore_{}",
            root.to_string_lossy()
                .to_lowercase()
                .replace(['\\', '/', ':'], "_")
        ))
    }
    pub fn new(root: &Path) -> Result<Self> {
        // SAFETY: Named auto-reset event belongs to the UI process and is closed by OwnedHandle.
        unsafe {
            let handle = CreateEventW(std::ptr::null(), 0, 0, Self::name(root).as_ptr());
            ensure!(!handle.is_null(), "无法建立窗口恢复事件");
            Ok(Self(OwnedHandle::from_raw_handle(handle)))
        }
    }
    pub fn requested(&self) -> bool {
        // SAFETY: The event handle remains owned and live.
        unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) == WAIT_OBJECT_0 }
    }
    pub fn request(root: &Path) -> bool {
        // SAFETY: Only signals the event for the selected environment; no foreign process is controlled.
        unsafe {
            let handle = OpenEventW(EVENT_MODIFY_STATE, 0, Self::name(root).as_ptr());
            if handle.is_null() {
                return false;
            }
            let handle = OwnedHandle::from_raw_handle(handle);
            SetEvent(handle.as_raw_handle()) != 0
        }
    }
}
impl Drop for AppLock {
    fn drop(&mut self) {
        /* SAFETY: Self owns this handle exactly once. */
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Identity {
    pub pid: u32,
    pub created: u64,
    pub executable: PathBuf,
}
pub struct Process {
    pub identity: Identity,
    handle: OwnedHandle,
}
/// Query only a PID recorded by this environment; inability to inspect it is not proof of exit.
pub(crate) fn recorded_process_exists(pid: u32) -> Result<bool> {
    // SAFETY: OpenProcess returns an owned handle; only query/synchronization access is requested.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
                return Ok(false);
            }
            return Err(error).context("无法核验本环境记录的进程状态");
        }
        let handle = OwnedHandle::from_raw_handle(handle);
        Ok(WaitForSingleObject(handle.as_raw_handle(), 0) == WAIT_TIMEOUT)
    }
}
impl Process {
    pub fn from_child(child: Child) -> Result<Self> {
        let pid = child.id();
        let result = Self::open(pid);
        drop(child);
        result
    }
    pub fn open(pid: u32) -> Result<Self> {
        // SAFETY: OpenProcess returns an owned handle; buffers are valid for the calls below.
        unsafe {
            let handle = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | SYNCHRONIZE,
                0,
                pid,
            );
            ensure!(!handle.is_null(), "无法访问本项目进程 {pid}");
            let handle = OwnedHandle::from_raw_handle(handle);
            let mut created = std::mem::zeroed();
            let mut exited = std::mem::zeroed();
            let mut kernel = std::mem::zeroed();
            let mut user = std::mem::zeroed();
            ensure!(
                GetProcessTimes(
                    handle.as_raw_handle(),
                    &mut created,
                    &mut exited,
                    &mut kernel,
                    &mut user
                ) != 0,
                "无法读取进程创建时间"
            );
            let mut path = vec![0u16; 32768];
            let mut size = path.len() as u32;
            ensure!(
                QueryFullProcessImageNameW(handle.as_raw_handle(), 0, path.as_mut_ptr(), &mut size)
                    != 0,
                "无法读取进程路径"
            );
            Ok(Self {
                identity: Identity {
                    pid,
                    created: ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64,
                    executable: PathBuf::from(String::from_utf16_lossy(&path[..size as usize])),
                },
                handle,
            })
        }
    }
    pub fn recover(identity: &Identity) -> Result<Self> {
        let p = Self::open(identity.pid)?;
        ensure!(
            p.identity.created == identity.created
                && p.identity
                    .executable
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&identity.executable.to_string_lossy()),
            "进程身份不匹配，拒绝接管"
        );
        Ok(p)
    }
    pub fn running(&self) -> bool {
        /* SAFETY: Owned process handle remains valid. */
        unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) == WAIT_TIMEOUT }
    }
    pub fn wait(&self, timeout: Duration) -> Result<()> {
        /* SAFETY: Owned process handle remains valid. */
        ensure!(
            unsafe {
                WaitForSingleObject(
                    self.handle.as_raw_handle(),
                    timeout.as_millis().min(u32::MAX as u128) as u32,
                )
            } == WAIT_OBJECT_0,
            "正常关闭超时，进程仍保留运行"
        );
        Ok(())
    }
    pub fn terminate_php(&self) -> Result<()> {
        /* SAFETY: Caller restricts termination to its own PHP worker. */
        ensure!(
            unsafe { TerminateProcess(self.handle.as_raw_handle(), 0) } != 0,
            "无法停止 PHP worker"
        );
        self.wait(Duration::from_secs(5))
    }
    pub fn owns_port(&self, port: u16) -> Result<bool> {
        if !self.running() {
            return Ok(false);
        }
        if let Some(pid) = listening_pid(port)? {
            if pid == self.identity.pid {
                return Ok(true);
            }
            if descendant(pid, self.identity.pid)? {
                let child = Self::open(pid)?;
                return Ok(child.running()
                    && child.identity.created >= self.identity.created
                    && child
                        .identity
                        .executable
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&self.identity.executable.to_string_lossy()));
            }
            return Ok(false);
        }
        Ok(false)
    }
    pub fn listener_process(&self, port: u16) -> Result<Self> {
        ensure!(self.owns_port(port)?, "端口监听者不属于本实例");
        let pid = listening_pid(port)?.context("监听进程已退出")?;
        let process = Self::open(pid)?;
        ensure!(
            process
                .identity
                .executable
                .to_string_lossy()
                .eq_ignore_ascii_case(&self.identity.executable.to_string_lossy()),
            "子进程可执行文件与实例不符"
        );
        ensure!(
            self.owns_port(port)? && listening_pid(port)? == Some(pid),
            "监听者已变化"
        );
        Ok(process)
    }
}
fn descendant(mut pid: u32, parent: u32) -> Result<bool> {
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    // SAFETY: Snapshot handle is owned; entry size is initialized per API contract.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        ensure!(snapshot != INVALID_HANDLE_VALUE, "无法校验子进程归属");
        let snapshot = OwnedHandle::from_raw_handle(snapshot);
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut parents = std::collections::HashMap::new();
        let mut ok = Process32FirstW(snapshot.as_raw_handle(), &mut entry);
        while ok != 0 {
            parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
            ok = Process32NextW(snapshot.as_raw_handle(), &mut entry);
        }
        for _ in 0..8 {
            match parents.get(&pid) {
                Some(p) if *p == parent => return Ok(true),
                Some(p) if *p != pid => pid = *p,
                _ => break,
            }
        }
    }
    Ok(false)
}
pub fn random_token() -> Result<String> {
    use windows_sys::Win32::Security::Cryptography::*;
    let mut bytes = [0u8; 16];
    // SAFETY: Buffer is writable and sized exactly; the system-preferred RNG needs no handle.
    ensure!(
        unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                bytes.as_mut_ptr(),
                bytes.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        } == 0,
        "随机数生成失败"
    );
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub fn system_hosts_path() -> Result<PathBuf> {
    let mut buffer = vec![0u16; 32768];
    // SAFETY: Writable UTF-16 buffer is sized for the Windows directory API.
    let len = unsafe {
        windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW(
            buffer.as_mut_ptr(),
            buffer.len() as u32,
        )
    };
    ensure!(
        len > 0 && (len as usize) < buffer.len(),
        "无法定位 Windows 系统目录"
    );
    Ok(
        PathBuf::from(String::from_utf16_lossy(&buffer[..len as usize]))
            .join("System32/drivers/etc/hosts"),
    )
}
pub fn elevate_hosts(exe: &Path, root: &Path, nonce: &str) -> Result<()> {
    use windows_sys::Win32::UI::{Shell::*, WindowsAndMessaging::SW_HIDE};
    let exe = wide(exe);
    let cwd = wide(root);
    let verb = wide("runas");
    let arguments = wide(format!("--apply-hosts \"{}\" {nonce}", root.display()));
    // SAFETY: Shell receives live UTF-16 buffers. The returned process handle is owned until wait completes.
    unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS;
        info.lpVerb = verb.as_ptr();
        info.lpFile = exe.as_ptr();
        info.lpParameters = arguments.as_ptr();
        info.lpDirectory = cwd.as_ptr();
        info.nShow = SW_HIDE;
        ensure!(
            ShellExecuteExW(&mut info) != 0,
            "提权请求取消或失败：{}",
            std::io::Error::last_os_error()
        );
        ensure!(!info.hProcess.is_null(), "未返回提权进程句柄");
        let process = OwnedHandle::from_raw_handle(info.hProcess);
        ensure!(
            WaitForSingleObject(process.as_raw_handle(), 30000) == WAIT_OBJECT_0,
            "hosts 助手尚未结束，请稍后查看结果"
        );
    }
    Ok(())
}

pub struct DirectoryWatch(HANDLE);
impl DirectoryWatch {
    pub fn new(path: &Path) -> Result<Self> {
        // SAFETY: The path is terminated and valid; this object owns the notification handle.
        let handle = unsafe {
            FindFirstChangeNotificationW(
                wide(path).as_ptr(),
                1,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_DIR_NAME
                    | FILE_NOTIFY_CHANGE_LAST_WRITE
                    | FILE_NOTIFY_CHANGE_SIZE,
            )
        };
        ensure!(
            handle != INVALID_HANDLE_VALUE && !handle.is_null(),
            "无法监测组件目录"
        );
        Ok(Self(handle))
    }
    pub fn changed(&self) -> Result<bool> {
        // SAFETY: The notification handle is owned and remains live until drop.
        unsafe {
            let changed = WaitForSingleObject(self.0, 0) == WAIT_OBJECT_0;
            if changed {
                ensure!(FindNextChangeNotification(self.0) != 0, "组件目录监测失效");
            }
            Ok(changed)
        }
    }
}
impl Drop for DirectoryWatch {
    fn drop(&mut self) {
        // SAFETY: The notification handle is released exactly once by its owner.
        unsafe {
            FindCloseChangeNotification(self.0);
        }
    }
}
