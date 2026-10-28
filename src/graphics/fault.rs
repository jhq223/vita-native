//! Shared terminal ownership fault and construction error handling.
use super::{DeviceFault, Error, types::Result};
use std::{cell::OnceCell, prelude::v1::*, rc::Rc, sync::Arc};
#[derive(Clone, Default)]
pub(super) struct Fault(Rc<OnceCell<Arc<DeviceFault>>>);
impl Fault {
    pub fn get(&self) -> Option<Arc<DeviceFault>> {
        self.0.get().cloned()
    }
    pub fn check(&self) -> Result<()> {
        self.get().map_or(Ok(()), |f| Err(Error::Terminal(f)))
    }
    pub fn record(&self, operation: &'static str, code: i32) -> Error {
        let f = self
            .0
            .get_or_init(|| Arc::new(DeviceFault { operation, code }));
        Error::Terminal(f.clone())
    }
    pub fn command(&self, operation: &'static str, code: i32) -> Result<()> {
        if code < 0 {
            Err(self.record(operation, code))
        } else {
            Ok(())
        }
    }
}
pub(super) fn sdk(operation: &'static str, code: i32) -> Result<()> {
    if code < 0 {
        Err(Error::Sdk(operation, code))
    } else {
        Ok(())
    }
}
