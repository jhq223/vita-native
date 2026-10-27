//! Owned system keyboard with explicit input-buffer lifetime.
//!
//! The SDK draws this keyboard independently of the application's framebuffer.
//! Poll on the opening thread; callbacks copy only into fixed retained buffers.
use super::{Error, sdk};
use std::{
    cell::UnsafeCell,
    marker::PhantomData,
    prelude::v1::*,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
};

const CAPACITY: usize =
    (sdk::SCE_IME_MAX_TEXT_LENGTH + sdk::SCE_IME_MAX_PREEDIT_LENGTH + 1) as usize;
static OPEN: AtomicBool = AtomicBool::new(false);

/// Latest complete UTF-16 editing buffer; indices count UTF-16 code units.
#[derive(Debug)]
pub struct Text {
    /// Complete text excluding the trailing NUL.
    pub value: Vec<u16>,
    /// Cursor offset measured in UTF-16 code units.
    pub caret: u32,
    /// First UTF-16 unit belonging to the uncommitted candidate.
    pub preedit_start: u32,
    /// Length of the uncommitted candidate in UTF-16 units.
    pub preedit_length: u32,
}
/// Coalesced text and terminal requests delivered by one native update.
#[derive(Debug, Default)]
pub struct Update {
    /// Latest complete edit or caret update, if one arrived.
    pub text: Option<Text>,
    /// The player pressed the keyboard's confirmation key.
    pub enter: bool,
    /// The player requested dismissal; the owner must call `close`.
    pub close: bool,
}
struct State {
    work: [u64; sdk::SCE_IME_WORK_BUFFER_SIZE as usize / 8],
    initial: [u16; CAPACITY],
    output: [u16; CAPACITY],
    snapshot: [u16; CAPACITY],
    length: usize,
    caret: u32,
    preedit_start: u32,
    preedit_length: u32,
    changed: bool,
    enter: bool,
    close: bool,
    invalid: bool,
}
// SAFETY: sceImeUpdate invokes the handler synchronously on the owner thread.
// Its UnsafeCell allocation survives close, and no Rust reference into State
// is held across native calls. Native text is copied before callback return.
unsafe extern "C" fn event(arg: *mut core::ffi::c_void, event: *const sdk::SceImeEventData) {
    let state = unsafe { &mut *(*arg.cast::<UnsafeCell<State>>()).get() };
    let event = unsafe { &*event };
    match event.id {
        sdk::SCE_IME_EVENT_UPDATE_TEXT => {
            let text = unsafe { event.param.text };
            if text.str_.is_null() {
                state.invalid = true;
                return;
            }
            let mut length = 0;
            while length < CAPACITY {
                let unit = unsafe { *text.str_.add(length) };
                if unit == 0 {
                    break;
                }
                state.snapshot[length] = unit;
                length += 1;
            }
            if length == CAPACITY
                || text.caretIndex as usize > length
                || (text.preeditIndex as usize)
                    .checked_add(text.preeditLength as usize)
                    .is_none_or(|end| end > length)
            {
                state.invalid = true;
                return;
            }
            state.length = length;
            state.caret = text.caretIndex;
            state.preedit_start = text.preeditIndex;
            state.preedit_length = text.preeditLength;
            state.changed = true;
        }
        sdk::SCE_IME_EVENT_UPDATE_CARET => {
            state.caret = unsafe { event.param.caretIndex };
            state.changed = true;
        }
        sdk::SCE_IME_EVENT_PRESS_ENTER => state.enter = true,
        sdk::SCE_IME_EVENT_PRESS_CLOSE => state.close = true,
        _ => {}
    }
}

/// Exclusive, thread-affine keyboard owning all native work and text storage.
/// `close` reports native failures. On an unclosed native failure, Drop retains
/// callback storage so native code can never observe freed memory.
pub struct Keyboard {
    state: Option<Box<UnsafeCell<State>>>,
    active: bool,
    module: bool,
    _thread: PhantomData<Rc<()>>,
}
impl Keyboard {
    /// Open a system keyboard with an initial UTF-8 editing segment (at most
    /// 2048 UTF-16 units). The system language configuration selects keyboards.
    pub fn open(initial: &str) -> Result<Self, Error> {
        let units: Vec<_> = initial
            .encode_utf16()
            .take(sdk::SCE_IME_MAX_TEXT_LENGTH as usize + 1)
            .collect();
        if units.len() > sdk::SCE_IME_MAX_TEXT_LENGTH as usize || units.contains(&0) {
            return Err(Error {
                operation: "IME initial text exceeds native bounds",
                code: -1,
            });
        }
        if OPEN.swap(true, Ordering::AcqRel) {
            return Err(Error {
                operation: "IME already owned",
                code: -1,
            });
        }
        let mut keyboard = Self {
            state: None,
            active: false,
            module: false,
            _thread: PhantomData,
        };
        let result = (|| {
            // SAFETY: SDK scalar module identity; ownership remains on this thread.
            let loaded = unsafe { sdk::sceSysmoduleIsLoaded(sdk::SCE_SYSMODULE_IME) };
            if loaded as u32 == sdk::SCE_SYSMODULE_ERROR_UNLOADED {
                Error::check("sceSysmoduleLoadModule IME", unsafe {
                    sdk::sceSysmoduleLoadModule(sdk::SCE_SYSMODULE_IME)
                })?;
                keyboard.module = true;
            } else {
                Error::check("sceSysmoduleIsLoaded IME", loaded)?;
            }
            let mut state = Box::new(UnsafeCell::new(State {
                work: [0; sdk::SCE_IME_WORK_BUFFER_SIZE as usize / 8],
                initial: [0; CAPACITY],
                output: [0; CAPACITY],
                snapshot: [0; CAPACITY],
                length: units.len(),
                caret: units.len() as u32,
                preedit_start: 0,
                preedit_length: 0,
                changed: false,
                enter: false,
                close: false,
                invalid: false,
            }));
            // SAFETY: zero initializes SDK reserved fields; pointers target the
            // stable allocation retained before any callback can be dispatched.
            let mut param: sdk::SceImeParam = unsafe { core::mem::zeroed() };
            let storage = state.get_mut();
            storage.initial[..units.len()].copy_from_slice(&units);
            storage.snapshot[..units.len()].copy_from_slice(&units);
            param.sdkVersion = sdk::PSP2_SDK_VERSION;
            param.type_ = sdk::SCE_IME_TYPE_DEFAULT;
            param.option = sdk::SCE_IME_OPTION_NO_AUTO_CAPITALIZATION;
            param.work = storage.work.as_mut_ptr().cast();
            param.initialText = storage.initial.as_mut_ptr();
            param.inputTextBuffer = storage.output.as_mut_ptr();
            param.maxTextLength = sdk::SCE_IME_MAX_TEXT_LENGTH;
            param.handler = Some(event);
            param.arg = (&*state as *const UnsafeCell<State>).cast_mut().cast();
            keyboard.state = Some(state);
            Error::check("sceImeOpen", unsafe { sdk::sceImeOpen(&param) })?;
            keyboard.active = true;
            Ok(())
        })();
        if let Err(error) = result {
            OPEN.store(false, Ordering::Release);
            return Err(error);
        }
        Ok(keyboard)
    }

    /// Dispatch native events and copy the latest edited buffer. Text updates
    /// are coalesced within one poll; Enter and Close remain explicit requests.
    pub fn poll(&mut self) -> Result<Update, Error> {
        if !self.active {
            return Ok(Update::default());
        }
        // SAFETY: this !Send owner retains all callback storage across the call.
        Error::check("sceImeUpdate", unsafe { sdk::sceImeUpdate() })?;
        let state = self.state.as_mut().unwrap().get_mut();
        if state.invalid || state.caret as usize > state.length {
            return Err(Error {
                operation: "IME returned invalid UTF-16 bounds",
                code: -1,
            });
        }
        let text = core::mem::take(&mut state.changed).then(|| Text {
            value: state.snapshot[..state.length].to_vec(),
            caret: state.caret,
            preedit_start: state.preedit_start,
            preedit_length: state.preedit_length,
        });
        Ok(Update {
            text,
            enter: core::mem::take(&mut state.enter),
            close: core::mem::take(&mut state.close),
        })
    }

    /// Replace the native editing segment, then put its caret at the specified
    /// UTF-16 unit offset. Storage is retained until the next update or close.
    pub fn set_text(&mut self, text: &str, caret: u32) -> Result<(), Error> {
        let units: Vec<_> = text
            .encode_utf16()
            .take(sdk::SCE_IME_MAX_TEXT_LENGTH as usize + 1)
            .collect();
        if !self.active
            || units.len() > sdk::SCE_IME_MAX_TEXT_LENGTH as usize
            || units.contains(&0)
            || caret as usize > units.len()
        {
            return Err(Error {
                operation: "IME text or caret exceeds native bounds",
                code: -1,
            });
        }
        let state = self.state.as_mut().unwrap().get_mut();
        state.initial.fill(0);
        state.initial[..units.len()].copy_from_slice(&units);
        state.snapshot[..units.len()].copy_from_slice(&units);
        state.length = units.len();
        state.caret = caret;
        state.preedit_start = 0;
        state.preedit_length = 0;
        state.changed = false;
        let pointer = state.initial.as_ptr();
        // SAFETY: retained owned text and a local scalar caret; both SDK calls
        // complete on the opening thread before Rust reborrows callback storage.
        Error::check("sceImeSetText", unsafe {
            sdk::sceImeSetText(pointer, units.len() as u32)
        })?;
        let position = sdk::SceImeCaret {
            x: 0,
            y: 0,
            height: 0,
            index: caret,
        };
        Error::check("sceImeSetCaret", unsafe { sdk::sceImeSetCaret(&position) })?;
        Ok(())
    }

    /// Update the editor's screen-space caret geometry in physical pixels.
    pub fn set_area(&mut self, area: [u32; 3]) -> Result<(), Error> {
        let geometry = sdk::SceImePreeditGeometry {
            x: area[0],
            y: area[1],
            height: area[2],
        };
        // SAFETY: the SDK copies this initialized geometry during the call.
        Error::check("sceImeSetPreeditGeometry", unsafe {
            sdk::sceImeSetPreeditGeometry(&geometry)
        })?;
        Ok(())
    }

    /// Close callbacks before releasing buffers or unloading an owned module.
    /// A failed close keeps ownership and permits the caller to retry explicitly.
    pub fn close(&mut self) -> Result<(), Error> {
        if self.active {
            // SAFETY: this instance owns the process's one active native keyboard.
            Error::check("sceImeClose", unsafe { sdk::sceImeClose() })?;
            self.active = false;
            OPEN.store(false, Ordering::Release);
        }
        self.state = None;
        if self.module {
            Error::check("sceSysmoduleUnloadModule IME", unsafe {
                sdk::sceSysmoduleUnloadModule(sdk::SCE_SYSMODULE_IME)
            })?;
            self.module = false;
        }
        Ok(())
    }
}
impl Drop for Keyboard {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            std::eprintln!("native keyboard cleanup: {error}");
            if self.active
                && let Some(state) = self.state.take()
            {
                core::mem::forget(state);
            }
        }
    }
}
