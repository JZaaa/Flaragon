//! Current-user Windows protection for recovery records; native configurations remain editable.
use crate::{Result, ensure};
use windows_sys::Win32::{Foundation::LocalFree, Security::Cryptography::*};

const MAGIC: &[u8] = b"FLARAGON-DPAPI-1\0";

pub fn seal(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut result = MAGIC.to_vec();
    result.extend(transform(bytes, true)?);
    Ok(result)
}

pub fn open(bytes: &[u8]) -> Result<Vec<u8>> {
    match bytes.strip_prefix(MAGIC) {
        Some(data) => transform(data, false),
        None => Ok(bytes.to_vec()),
    }
}

fn transform(bytes: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    ensure!(bytes.len() <= 128 * 1024 * 1024, "受保护记录超过大小限制");
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: Input is valid for the call; DPAPI allocates output with LocalAlloc.
    // No UI is allowed, and LocalFree releases the returned buffer after copying it.
    unsafe {
        let ok = if encrypt {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        ensure!(
            ok != 0,
            "无法保护或读取恢复记录，请使用原 Windows 用户：{}",
            std::io::Error::last_os_error()
        );
        let result = if output.cbData == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec()
        };
        LocalFree(output.pbData.cast());
        Ok(result)
    }
}
