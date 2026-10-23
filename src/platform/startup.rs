use crate::{Context, Result, ensure, platform::wide};
use std::ptr::{null, null_mut};
use windows_sys::Win32::{Foundation::*, System::Registry::*};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: This wrapper exclusively owns the opened registry handle.
        unsafe { RegCloseKey(self.0) };
    }
}

fn allowed() -> Result<()> {
    ensure!(
        std::env::var_os("FLARAGON_DEV_ROOT").is_none(),
        "开发隔离模式禁止访问系统启动项"
    );
    Ok(())
}

fn checked(code: u32, operation: &str) -> Result<()> {
    if code != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(code as i32)).context(operation.to_owned());
    }
    Ok(())
}

fn open(write: bool, create: bool) -> Result<Option<Key>> {
    allowed()?;
    let mut key = null_mut();
    let access = KEY_QUERY_VALUE | KEY_WOW64_64KEY | if write { KEY_SET_VALUE } else { 0 };
    // SAFETY: The terminated subkey buffer and output pointer remain live for each call.
    let result = unsafe {
        if create {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                wide(RUN_KEY).as_ptr(),
                0,
                null(),
                REG_OPTION_NON_VOLATILE,
                access,
                null(),
                &mut key,
                null_mut(),
            )
        } else {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                wide(RUN_KEY).as_ptr(),
                0,
                access,
                &mut key,
            )
        }
    };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    checked(result, "无法打开当前用户的 Windows 登录启动设置")?;
    Ok(Some(Key(key)))
}

pub(crate) fn read(name: &str) -> Result<Option<String>> {
    let Some(key) = open(false, false)? else {
        return Ok(None);
    };
    let mut data = [0u16; 1024];
    let mut size = std::mem::size_of_val(&data) as u32;
    // SAFETY: Key is live; the writable UTF-16 buffer has the supplied byte capacity.
    let result = unsafe {
        RegGetValueW(
            key.0,
            null(),
            wide(name).as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            data.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    checked(result, "无法读取本环境的 Windows 登录启动项")?;
    ensure!(
        size >= 2 && size.is_multiple_of(2) && size as usize <= std::mem::size_of_val(&data),
        "登录启动项格式无效"
    );
    let data = &data[..size as usize / 2];
    ensure!(
        data.last() == Some(&0) && !data[..data.len() - 1].contains(&0),
        "登录启动项格式无效"
    );
    Ok(Some(
        String::from_utf16(&data[..data.len() - 1]).context("登录启动项编码无效")?,
    ))
}

pub(crate) fn write(name: &str, command: Option<&str>) -> Result<()> {
    let Some(key) = open(true, command.is_some())? else {
        return Ok(());
    };
    // SAFETY: Key and terminated buffers are live; byte count includes the UTF-16 terminator.
    let result = unsafe {
        if let Some(command) = command {
            let data = wide(command);
            RegSetValueExW(
                key.0,
                wide(name).as_ptr(),
                0,
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        } else {
            RegDeleteValueW(key.0, wide(name).as_ptr())
        }
    };
    if command.is_none() && result == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    checked(result, "无法更新本环境的 Windows 登录启动项")
}
