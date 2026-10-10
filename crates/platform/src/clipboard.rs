//! The OLE clipboard as Explorer uses it: a `CF_HDROP` file list plus the shell's
//! `Preferred DropEffect` word that distinguishes 剪切 (move) from 复制 (copy).

use crate::bindings::*;
use crate::dragdrop;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use windows_core::Interface;

static PREFERRED_EFFECT_FORMAT: OnceLock<u16> = OnceLock::new();
thread_local! { static OWNED_DATA: RefCell<Option<IDataObject>> = const { RefCell::new(None) }; }

/// Put the shell's file data object on the clipboard without constructing every shell
/// extension's context menu. Explorer receives the original PIDLs and preferred effect.
pub fn set_files(paths: &[&Path], cut: bool) -> windows_core::Result<()> {
    let data = dragdrop::data_object_for_paths(paths)?;
    let fmt = dragdrop::hglobal_format(dragdrop::registered_format(
        "Preferred DropEffect",
        &PREFERRED_EFFECT_FORMAT,
    ));
    windows_core::link!("kernel32.dll" "system" fn GlobalAlloc(flags: u32, bytes: usize) -> HGLOBAL);
    windows_core::link!("kernel32.dll" "system" fn GlobalFree(memory: HGLOBAL) -> HGLOBAL);
    // SAFETY: SetData takes the allocation on success. Each failure frees it locally;
    // the shell data object and OLE clipboard manage their own COM references.
    unsafe {
        let memory = GlobalAlloc(0x0042, size_of::<u32>());
        if memory.0.is_null() {
            return Err(windows_core::Error::from_hresult(windows_core::HRESULT(
                0x8007000eu32 as i32,
            )));
        }
        let ptr = GlobalLock(memory) as *mut u32;
        if ptr.is_null() {
            let _ = GlobalFree(memory);
            return Err(windows_core::Error::from_hresult(windows_core::HRESULT(
                0x8007000eu32 as i32,
            )));
        }
        *ptr = if cut {
            DROPEFFECT_MOVE as u32
        } else {
            DROPEFFECT_COPY as u32
        };
        let _ = GlobalUnlock(memory);
        let medium = STGMEDIUM {
            tymed: TYMED_HGLOBAL as u32,
            Anonymous: uSTGMEDIUM_0 { hGlobal: memory },
            pUnkForRelease: core::mem::ManuallyDrop::new(None),
        };
        if let Err(error) = data.SetData(&fmt, &medium, true).ok() {
            let _ = GlobalFree(memory);
            return Err(error);
        }
        OleSetClipboard(&data).ok()?;
        OWNED_DATA.with(|slot| *slot.borrow_mut() = Some(data));
        Ok(())
    }
}

/// Files currently on the clipboard and whether they were cut (`true`) or copied (`false`).
/// A missing `Preferred DropEffect` counts as copy, as in Explorer.
pub fn file_list() -> Option<(Vec<PathBuf>, bool)> {
    // SAFETY: plain OLE call; the returned object is released when dropped.
    let data = unsafe { OleGetClipboard() }.ok()?;
    if !dragdrop::has_hdrop(&data) {
        return None;
    }
    let paths = dragdrop::hdrop_paths(&data);
    if paths.is_empty() {
        return None;
    }
    let fmt = dragdrop::hglobal_format(dragdrop::registered_format(
        "Preferred DropEffect",
        &PREFERRED_EFFECT_FORMAT,
    ));
    let effect = dragdrop::hglobal_bytes(&data, &fmt)
        .filter(|b| b.len() >= 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .unwrap_or(DROPEFFECT_COPY as u32);
    let cut = effect & DROPEFFECT_MOVE as u32 != 0 && effect & DROPEFFECT_COPY as u32 == 0;
    Some((paths, cut))
}

/// Cheap check for menu enabling: is there a file list on the clipboard?
pub fn has_file_list() -> bool {
    // SAFETY: plain OLE call.
    unsafe { OleGetClipboard() }
        .map(|d| dragdrop::has_hdrop(&d))
        .unwrap_or(false)
}

/// Empties the clipboard (Explorer does this after pasting cut files).
pub fn clear() {
    // SAFETY: plain OLE call; a null object empties the clipboard.
    unsafe {
        let _ = OleSetClipboard(None::<&IDataObject>);
    }
}

/// `GetClipboardSequenceNumber`: changes whenever any program changes the clipboard.
pub fn sequence() -> u32 {
    // SAFETY: plain FFI call.
    unsafe { GetClipboardSequenceNumber() }
}

/// Materialize our OLE file data before the UI thread and its COM apartment disappear.
pub fn flush() {
    windows_core::link!("ole32.dll" "system" fn OleFlushClipboard() -> windows_core::HRESULT);
    windows_core::link!("ole32.dll" "system" fn OleIsCurrentClipboard(data: *mut core::ffi::c_void) -> windows_core::HRESULT);
    OWNED_DATA.with(|slot| {
        if let Some(data) = slot.borrow_mut().take() {
            // SAFETY: our COM reference is live on this STA. Only materialize our own data.
            unsafe {
                if OleIsCurrentClipboard(data.as_raw()).0 == 0 {
                    let _ = OleFlushClipboard();
                }
            }
        }
    });
}
