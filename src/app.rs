//! Owned Vita services. SDK declarations are centralized in this module.
use std::prelude::v1::*;
pub(crate) use vitasdk_sys as sdk;
pub mod fs;
pub mod ime;
pub mod input;
pub mod memory;
pub mod power;
pub mod time;
pub mod wake;

/// An SDK failure with its original native error code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error {
    pub operation: &'static str,
    pub code: i32,
}
impl Error {
    pub(crate) fn check(operation: &'static str, code: i32) -> Result<i32, Self> {
        if code < 0 {
            Err(Self { operation, code })
        } else {
            Ok(code)
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {:#010x}", self.operation, self.code)
    }
}
impl std::error::Error for Error {}

/// The installed application's stable title identity, used for persistent data.
pub fn title_id() -> Result<std::string::String, Error> {
    let mut value = [0_u8; 16];
    // SDK SCE_APPMGR_APP_PARAM_TITLE_ID is 12 (older bindings expose the function
    // but omit this enumeration). The SDK copies at most the provided capacity.
    Error::check("sceAppMgrAppParamGetString TITLE_ID", unsafe {
        sdk::sceAppMgrAppParamGetString(
            sdk::sceKernelGetProcessId(),
            12,
            value.as_mut_ptr().cast(),
            value.len() as u32,
        )
    })?;
    let length = value
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(value.len());
    let value = &value[..length];
    if value.len() != 9 || !value.iter().all(u8::is_ascii_alphanumeric) {
        return Err(Error {
            operation: "invalid installed title identity",
            code: -1,
        });
    }
    std::string::String::from_utf8(value.to_vec()).map_err(|_| Error {
        operation: "invalid title identity encoding",
        code: -1,
    })
}
