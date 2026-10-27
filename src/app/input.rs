//! Fixed-capacity controller and front-touch samples.
use super::{Error, sdk};
use std::{marker::PhantomData, prelude::v1::*, rc::Rc};

/// Native UP button mask in a sampled button set.
pub const UP: u32 = sdk::SCE_CTRL_UP;
/// Native DOWN button mask in a sampled button set.
pub const DOWN: u32 = sdk::SCE_CTRL_DOWN;
/// Native LEFT button mask in a sampled button set.
pub const LEFT: u32 = sdk::SCE_CTRL_LEFT;
/// Native RIGHT button mask in a sampled button set.
pub const RIGHT: u32 = sdk::SCE_CTRL_RIGHT;
/// Native CROSS button mask in a sampled button set.
pub const CROSS: u32 = sdk::SCE_CTRL_CROSS;
/// Native CIRCLE button mask in a sampled button set.
pub const CIRCLE: u32 = sdk::SCE_CTRL_CIRCLE;
/// Native SQUARE button mask in a sampled button set.
pub const SQUARE: u32 = sdk::SCE_CTRL_SQUARE;
/// Native TRIANGLE button mask in a sampled button set.
pub const TRIANGLE: u32 = sdk::SCE_CTRL_TRIANGLE;
/// Native L button mask in a sampled button set.
pub const L: u32 = sdk::SCE_CTRL_LTRIGGER;
/// Native R button mask in a sampled button set.
pub const R: u32 = sdk::SCE_CTRL_RTRIGGER;
/// Native START button mask in a sampled button set.
pub const START: u32 = sdk::SCE_CTRL_START;
/// Native SELECT button mask in a sampled button set.
pub const SELECT: u32 = sdk::SCE_CTRL_SELECT;
/// Maximum front-panel contacts copied into one sample.
pub const MAX_TOUCHES: usize = 6;

/// A stable front contact with normalized display coordinates and pressure.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Touch {
    /// SDK identity retained for the lifetime of this contact.
    pub id: u8,
    /// Display X/Y in 0..=1, with the origin at the upper left.
    pub position: [f32; 2],
    /// Pressure normalized using the actual panel force range.
    pub force: f32,
}
/// Fixed-capacity latest controller and front-touch state.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sample {
    /// Pressed SDK button masks.
    pub buttons: u32,
    /// Left X/Y and right X/Y in -1..=1; positive Y points down.
    pub axes: [f32; 4],
    /// Display-normalized front contacts, with stable SDK contact identities.
    pub touches: [Touch; MAX_TOUCHES],
    /// Valid prefix of `touches`.
    pub touch_count: usize,
}
/// Thread-affine owner of analog controller and front-panel sampling.
pub struct Input {
    panel: sdk::SceTouchPanelInfo,
    last: Sample,
    _thread: PhantomData<Rc<()>>,
}
impl Input {
    /// Start sampling and query the actual front-panel bounds.
    pub fn new() -> Result<Self, Error> {
        // SAFETY: SDK scalar mode and exclusive initialized output.
        unsafe {
            Error::check(
                "sceCtrlSetSamplingMode",
                sdk::sceCtrlSetSamplingMode(sdk::SCE_CTRL_MODE_ANALOG),
            )?;
            Error::check(
                "sceTouchSetSamplingState",
                sdk::sceTouchSetSamplingState(
                    sdk::SCE_TOUCH_PORT_FRONT,
                    sdk::SCE_TOUCH_SAMPLING_STATE_START,
                ),
            )?;
            let mut panel = std::mem::zeroed();
            if let Err(error) = Error::check(
                "sceTouchGetPanelInfo",
                sdk::sceTouchGetPanelInfo(sdk::SCE_TOUCH_PORT_FRONT, &mut panel),
            ) {
                sdk::sceTouchSetSamplingState(
                    sdk::SCE_TOUCH_PORT_FRONT,
                    sdk::SCE_TOUCH_SAMPLING_STATE_STOP,
                );
                return Err(error);
            }
            let input = Self {
                panel,
                last: Sample::default(),
                _thread: PhantomData,
            };
            if input.panel.maxDispX <= input.panel.minDispX
                || input.panel.maxDispY <= input.panel.minDispY
            {
                return Err(Error {
                    operation: "invalid touch panel geometry",
                    code: -1,
                });
            }
            Ok(input)
        }
    }
    /// Peek the latest SDK data without waiting or allocating.
    pub fn sample(&mut self) -> Result<Sample, Error> {
        // SAFETY: both fixed SDK structs are initialized and exclusively written.
        unsafe {
            let mut pad: sdk::SceCtrlData = std::mem::zeroed();
            let mut touch: sdk::SceTouchData = std::mem::zeroed();
            if Error::check(
                "sceCtrlPeekBufferPositive",
                sdk::sceCtrlPeekBufferPositive(0, &mut pad, 1),
            )? > 0
            {
                self.last.buttons = pad.buttons;
                let axis = |v: u8| {
                    if v >= 128 {
                        f32::from(v - 128) / 127.
                    } else {
                        -(f32::from(128 - v) / 128.)
                    }
                };
                self.last.axes = [axis(pad.lx), axis(pad.ly), axis(pad.rx), axis(pad.ry)];
            }
            if Error::check(
                "sceTouchPeek",
                sdk::sceTouchPeek(sdk::SCE_TOUCH_PORT_FRONT, &mut touch, 1),
            )? > 0
            {
                self.last.touch_count = (touch.reportNum as usize).min(MAX_TOUCHES);
                for (out, report) in self
                    .last
                    .touches
                    .iter_mut()
                    .zip(&touch.report)
                    .take(self.last.touch_count)
                {
                    let map = |v: i16, low: i16, high: i16| {
                        ((f32::from(v) - f32::from(low)) / f32::from(high - low)).clamp(0., 1.)
                    };
                    *out = Touch {
                        id: report.id,
                        position: [
                            map(report.x, self.panel.minDispX, self.panel.maxDispX),
                            map(report.y, self.panel.minDispY, self.panel.maxDispY),
                        ],
                        force: f32::from(report.force.saturating_sub(self.panel.minForce))
                            / f32::from(
                                self.panel
                                    .maxForce
                                    .saturating_sub(self.panel.minForce)
                                    .max(1),
                            ),
                    };
                }
            }
            Ok(self.last)
        }
    }
}
impl Drop for Input {
    fn drop(&mut self) {
        // SAFETY: this instance owns sampling; no native pointers escape it.
        unsafe {
            sdk::sceTouchSetSamplingState(
                sdk::SCE_TOUCH_PORT_FRONT,
                sdk::SCE_TOUCH_SAMPLING_STATE_STOP,
            );
        }
    }
}
