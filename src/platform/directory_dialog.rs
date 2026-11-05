//! Explorer-style directory selection with visible files and an editable address bar.
use crate::{Context, Result, config, ensure};
use std::{
    ffi::OsString,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use windows::{
    Win32::{
        Foundation::{E_ACCESSDENIED, E_FAIL, ERROR_CANCELLED, HWND, S_OK},
        System::Com::*,
        UI::Shell::*,
    },
    core::{HRESULT, Interface, PCWSTR, Ref, implement, w},
};

const SELECT_FOLDER: u32 = 100;

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: Created only after successful COM initialization on this same thread.
        unsafe { CoUninitialize() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_rejects_escape_before_reading_target() {
        let boundary = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(validate_navigation(boundary, Some(boundary)).is_ok());
        assert!(validate_navigation(&boundary.join("..\\outside"), Some(boundary)).is_err());
        let similar = PathBuf::from(format!("{}-other", boundary.display()));
        assert!(validate_navigation(&similar, Some(boundary)).is_err());
    }

    #[test]
    fn empty_initial_folder_uses_shell_default_or_development_boundary() {
        let boundary = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(initial_folder(Path::new(""), None).unwrap(), None);
        assert_eq!(
            initial_folder(Path::new(""), Some(boundary)).unwrap(),
            Some(boundary.to_path_buf())
        );
        let missing = boundary.join(format!(
            "picker-missing-{}",
            crate::platform::random_token().unwrap()
        ));
        assert_eq!(
            initial_folder(&missing, Some(boundary)).unwrap(),
            Some(boundary.to_path_buf())
        );
        assert!(initial_folder(Path::new("relative"), Some(boundary)).is_err());
        assert!(initial_folder(&boundary.join("..\\outside"), Some(boundary)).is_err());
    }
}

fn shell_path(item: &IShellItem) -> windows::core::Result<PathBuf> {
    // SAFETY: The live Shell item returns a COM-allocated, terminated UTF-16 string.
    unsafe {
        let name = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = PathBuf::from(OsString::from_wide(name.as_wide()));
        CoTaskMemFree(Some(name.0.cast()));
        Ok(path)
    }
}

fn validate_navigation(path: &Path, boundary: Option<&Path>) -> Result<()> {
    let path = config::local_path(path, path)?;
    if let Some(boundary) = boundary {
        ensure!(config::path_within(&path, boundary), "目录超出开发隔离范围");
    }
    config::reject_links(&path)
}

fn initial_folder(initial: &Path, boundary: Option<&Path>) -> Result<Option<PathBuf>> {
    let initial = if initial.as_os_str().is_empty() {
        boundary
    } else {
        Some(initial)
    };
    initial
        .map(|path| {
            validate_navigation(path, boundary)?;
            let folder = path
                .ancestors()
                .find(|path| path.is_dir())
                .context("找不到初始目录")?;
            validate_navigation(folder, boundary)?;
            Ok(folder.to_path_buf())
        })
        .transpose()
}

#[implement(IFileDialogEvents, IFileDialogControlEvents)]
struct Events {
    boundary: Option<PathBuf>,
    selected: Arc<Mutex<Option<PathBuf>>>,
}

impl Events {
    fn select(&self, folder: &IShellItem) -> windows::core::Result<()> {
        let path = shell_path(folder)?;
        validate_navigation(&path, self.boundary.as_deref())
            .map_err(|_| windows::core::Error::from_hresult(E_ACCESSDENIED))?;
        *self
            .selected
            .lock()
            .map_err(|_| windows::core::Error::from_hresult(E_FAIL))? = Some(path);
        Ok(())
    }
}

impl IFileDialogEvents_Impl for Events_Impl {
    fn OnFileOk(&self, dialog: Ref<IFileDialog>) -> windows::core::Result<()> {
        // SAFETY: Callback references remain valid for this synchronous invocation.
        let folder = unsafe { dialog.ok()?.GetResult()?.GetParent()? };
        self.select(&folder)
    }
    fn OnFolderChanging(
        &self,
        _: Ref<IFileDialog>,
        folder: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        let path = shell_path(folder.ok()?)?;
        validate_navigation(&path, self.boundary.as_deref())
            .map_err(|_| windows::core::Error::from_hresult(E_ACCESSDENIED))
    }
    fn OnFolderChange(&self, _: Ref<IFileDialog>) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnSelectionChange(&self, _: Ref<IFileDialog>) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnTypeChange(&self, _: Ref<IFileDialog>) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnShareViolation(
        &self,
        _: Ref<IFileDialog>,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<FDE_SHAREVIOLATION_RESPONSE> {
        Ok(FDESVR_DEFAULT)
    }
    fn OnOverwrite(
        &self,
        _: Ref<IFileDialog>,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<FDE_OVERWRITE_RESPONSE> {
        Ok(FDEOR_DEFAULT)
    }
}

impl IFileDialogControlEvents_Impl for Events_Impl {
    fn OnButtonClicked(
        &self,
        customize: Ref<IFileDialogCustomize>,
        control: u32,
    ) -> windows::core::Result<()> {
        if control == SELECT_FOLDER {
            let dialog: IFileDialog = customize.ok()?.cast()?;
            // SAFETY: The callback owns a live COM reference; closing completes the modal Show call.
            unsafe {
                self.select(&dialog.GetFolder()?)?;
                dialog.Close(S_OK)?;
            }
        }
        Ok(())
    }
    fn OnItemSelected(
        &self,
        _: Ref<IFileDialogCustomize>,
        _: u32,
        _: u32,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnCheckButtonToggled(
        &self,
        _: Ref<IFileDialogCustomize>,
        _: u32,
        _: windows::core::BOOL,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnControlActivating(
        &self,
        _: Ref<IFileDialogCustomize>,
        _: u32,
    ) -> windows::core::Result<()> {
        Ok(())
    }
}

/// The caller keeps the owner alive and validates the selected directory for its purpose.
pub(super) fn choose(
    owner: isize,
    initial: &Path,
    boundary: Option<&Path>,
    title: &str,
) -> Result<Option<PathBuf>> {
    let initial = initial_folder(initial, boundary)?;
    // SAFETY: This worker owns its COM apartment and every interface until Show completes.
    // UTF-16 arguments are valid for each synchronous call; the UI keeps the owner alive.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .context("无法初始化目录选择器")?;
        let _apartment = Apartment;
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetOptions(
            FOS_FORCEFILESYSTEM
                | FOS_PATHMUSTEXIST
                | FOS_FILEMUSTEXIST
                | FOS_NOCHANGEDIR
                | FOS_DONTADDTORECENT
                | FOS_NOTESTFILECREATE
                | FOS_NODEREFERENCELINKS,
        )?;
        dialog.SetTitle(PCWSTR(super::wide(title).as_ptr()))?;
        dialog.SetOkButtonLabel(w!("使用文件所在目录"))?;
        let customize: IFileDialogCustomize = dialog.cast()?;
        customize.AddPushButton(SELECT_FOLDER, w!("使用当前目录"))?;
        customize.MakeProminent(SELECT_FOLDER)?;
        if let Some(boundary) = boundary {
            let root: IShellItem =
                SHCreateItemFromParsingName(PCWSTR(super::wide(boundary).as_ptr()), None)?;
            dialog.cast::<IFileDialog2>()?.SetNavigationRoot(&root)?;
        }
        if let Some(initial) = initial {
            let folder: IShellItem =
                SHCreateItemFromParsingName(PCWSTR(super::wide(initial).as_ptr()), None)?;
            dialog.SetFolder(&folder)?;
        }
        let selected = Arc::new(Mutex::new(None));
        let events: IFileDialogEvents = Events {
            boundary: boundary.map(Path::to_path_buf),
            selected: selected.clone(),
        }
        .into();
        let cookie = dialog.Advise(&events)?;
        let result = dialog.Show(Some(HWND(owner as *mut _)));
        dialog.Unadvise(cookie)?;
        if let Err(error) = result {
            if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0) {
                return Ok(None);
            }
            return Err(error).context("无法选择目录");
        }
        let path = selected
            .lock()
            .map_err(|_| crate::anyhow!("目录选择状态不可用"))?
            .take()
            .context("未取得选择的目录")?;
        validate_navigation(&path, boundary)?;
        Ok(Some(path))
    }
}
