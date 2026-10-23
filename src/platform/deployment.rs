use crate::{
    Context, Result, ensure,
    platform::{Identity, wide},
};
use std::{
    fs,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::Path,
};
use windows_sys::Win32::{
    Foundation::*,
    Storage::FileSystem::*,
    System::{LibraryLoader::*, Threading::*},
};

pub fn free_space(path: &Path) -> Result<u64> {
    let mut available = 0;
    // SAFETY: The terminated path and writable output remain live for the synchronous call.
    ensure!(
        unsafe {
            GetDiskFreeSpaceExW(
                wide(path).as_ptr(),
                &mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } != 0,
        "无法检查磁盘空间：{}",
        std::io::Error::last_os_error()
    );
    Ok(available)
}

/// Keeps the selected directory from being renamed while a deployment is in progress.
pub fn directory_guard(path: &Path) -> Result<(fs::File, [u32; 3])> {
    use std::os::windows::fs::OpenOptionsExt;
    crate::config::reject_links(path)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: The file owns a live directory handle and info is a writable output buffer.
    ensure!(
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } != 0,
        "无法核验安装目录身份"
    );
    Ok((
        file,
        [
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        ],
    ))
}

/// Reads PE metadata without loading or executing the selected program.
pub fn check_program_identity(path: &Path, expected: &str) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    crate::config::reject_links(path)?;
    let mut file = fs::File::open(path)?;
    let mut dos = [0u8; 64];
    file.read_exact(&mut dos)?;
    ensure!(&dos[..2] == b"MZ", "目标不是有效的 Flaragon 程序");
    let offset = u32::from_le_bytes(dos[60..64].try_into()?);
    file.seek(SeekFrom::Start(offset as u64))?;
    let mut pe = [0u8; 26];
    file.read_exact(&mut pe)?;
    ensure!(
        &pe[..4] == b"PE\0\0"
            && u16::from_le_bytes(pe[4..6].try_into()?) == 0x8664
            && u16::from_le_bytes(pe[24..26].try_into()?) == 0x20b,
        "目标不是 Windows x64 程序"
    );
    // SAFETY: Version APIs read the file resource into an owned aligned buffer. Returned string
    // pointers are range-checked against that buffer before being copied; no target code executes.
    unsafe {
        let size = GetFileVersionInfoSizeW(wide(path).as_ptr(), std::ptr::null_mut());
        ensure!(size > 0 && size <= 1024 * 1024, "程序缺少有效产品版本信息");
        let mut buffer = vec![0u32; (size as usize).div_ceil(4)];
        ensure!(
            GetFileVersionInfoW(wide(path).as_ptr(), 0, size, buffer.as_mut_ptr().cast()) != 0,
            "无法读取程序版本信息"
        );
        for (key, expected) in [
            ("ProductName", "Flaragon"),
            ("ProductVersion", expected),
            ("OriginalFilename", "Flaragon.exe"),
        ] {
            let mut pointer = std::ptr::null_mut();
            let mut length = 0;
            ensure!(
                VerQueryValueW(
                    buffer.as_ptr().cast(),
                    wide(format!(r"\StringFileInfo\040904B0\{key}")).as_ptr(),
                    &mut pointer,
                    &mut length
                ) != 0,
                "程序缺少产品标识：{key}"
            );
            let start = pointer as usize;
            let base = buffer.as_ptr() as usize;
            let bytes = (length as usize)
                .checked_mul(2)
                .context("版本信息长度无效")?;
            ensure!(
                length > 0
                    && start >= base
                    && start.is_multiple_of(2)
                    && start
                        .checked_add(bytes)
                        .is_some_and(|end| end <= base + size as usize),
                "版本信息指针无效"
            );
            let value = String::from_utf16(std::slice::from_raw_parts(
                pointer.cast::<u16>(),
                length as usize,
            ))?;
            ensure!(
                value.trim_end_matches('\0') == expected,
                "软件产品或版本与发布清单不符：{key}"
            );
        }
    }
    Ok(())
}

/// Only queries the recorded PID. It never signals, connects to, or terminates a service.
pub fn recorded_process_running(identity: &Identity) -> Result<bool> {
    // SAFETY: The handle is owned by the guard; all output buffers have their declared capacities.
    unsafe {
        let handle = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE,
            0,
            identity.pid,
        );
        if handle.is_null() {
            let code = GetLastError();
            if code == ERROR_INVALID_PARAMETER {
                return Ok(false);
            }
            crate::bail!(
                "无法核验记录进程 {}，已终止更新：{}",
                identity.pid,
                std::io::Error::from_raw_os_error(code as i32)
            );
        }
        let handle = OwnedHandle::from_raw_handle(handle);
        if WaitForSingleObject(handle.as_raw_handle(), 0) == WAIT_OBJECT_0 {
            return Ok(false);
        }
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
            "无法核验进程启动时间"
        );
        let stamp = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
        if stamp != identity.created {
            return Ok(false);
        }
        let mut path = vec![0u16; 32768];
        let mut len = path.len() as u32;
        ensure!(
            QueryFullProcessImageNameW(handle.as_raw_handle(), 0, path.as_mut_ptr(), &mut len) != 0,
            "无法核验进程路径"
        );
        let actual = String::from_utf16(&path[..len as usize])?;
        ensure!(
            actual.eq_ignore_ascii_case(&identity.executable.to_string_lossy()),
            "进程身份不一致，已终止更新"
        );
        Ok(true)
    }
}

pub fn check_editor_dll(path: &Path, export: &[u8]) -> Result<()> {
    crate::config::reject_links(path)?;
    ensure!(fs::metadata(path)?.is_file(), "编辑器依赖缺失");
    // SAFETY: The absolute library path is validated; the exported symbol is only resolved, not called.
    // The trailing NUL is supplied by the static caller. The module is freed before returning.
    unsafe {
        let module = LoadLibraryExW(
            wide(path).as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        );
        ensure!(
            !module.is_null(),
            "无法加载编辑器依赖 {}：{}",
            path.display(),
            std::io::Error::last_os_error()
        );
        let found = GetProcAddress(module, export.as_ptr()).is_some();
        FreeLibrary(module);
        ensure!(found, "编辑器依赖接口不兼容：{}", path.display());
    }
    Ok(())
}

pub fn ensure_files_free(root: &Path, names: impl Iterator<Item = String>) -> Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    for name in names {
        let path = root.join(name);
        crate::config::reject_links(&path)?;
        if path.exists() {
            let _file = fs::OpenOptions::new().read(true).write(true).share_mode(0).open(&path)
                .with_context(|| format!("程序文件正在使用或不可写：{}。请先“停止并退出”后重新运行升级包，已终止更新", path.display()))?;
        }
    }
    Ok(())
}

pub fn choose_directory(initial: &Path) -> Result<Option<std::path::PathBuf>> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{System::Com::*, UI::Shell::*};
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            /* SAFETY: Balances this thread's successful initialization. */
            unsafe {
                CoUninitialize();
            }
        }
    }
    struct Pidl(*mut Common::ITEMIDLIST);
    impl Drop for Pidl {
        fn drop(&mut self) {
            /* SAFETY: The shell allocated this PIDL with the COM allocator. */
            unsafe {
                CoTaskMemFree(self.0.cast());
            }
        }
    }
    let boundary = std::env::var_os("FLARAGON_DEV_ROOT").map(std::path::PathBuf::from);
    if let Some(path) = &boundary {
        crate::config::reject_links(path)?;
    }
    let _ = initial;
    // SAFETY: COM is initialized on this worker; PIDLs are owned, output buffers remain live.
    unsafe {
        ensure!(
            CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) >= 0,
            "无法初始化目录选择器"
        );
        let _apartment = Apartment;
        let mut root = Pidl(std::ptr::null_mut());
        if let Some(boundary) = boundary {
            ensure!(
                SHParseDisplayName(
                    wide(&boundary).as_ptr(),
                    std::ptr::null_mut(),
                    &mut root.0,
                    0,
                    std::ptr::null_mut()
                ) >= 0,
                "无法打开目录范围"
            );
        }
        let title = wide("选择 Flaragon 使用目录");
        let mut display = [0u16; 260];
        let info = BROWSEINFOW {
            hwndOwner: std::ptr::null_mut(),
            pidlRoot: root.0,
            pszDisplayName: display.as_mut_ptr(),
            lpszTitle: title.as_ptr(),
            ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE | BIF_EDITBOX,
            lpfn: None,
            lParam: 0,
            iImage: 0,
        };
        let selected = Pidl(SHBrowseForFolderW(&info));
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
            "请选择本机目录"
        );
        let len = path.iter().position(|v| *v == 0).context("目录路径无效")?;
        let path = std::path::PathBuf::from(std::ffi::OsString::from_wide(&path[..len]));
        Ok(Some(crate::deployment::target_root(&path)?))
    }
}
