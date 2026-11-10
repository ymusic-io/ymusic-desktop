//! The surface YouTube video presents into: an `AVSampleBufferDisplayLayer` added over the gpui
//! window's own layer and kept on the video box's bounds. Above gpui's drawing, so nothing gpui
//! paints can overlap the box while it shows.

use std::ffi::c_void;

use gpui_kit::{Bounds, Pixels, Window};

#[cfg(target_os = "macos")]
pub struct VideoLayer {
    layer: objc2::rc::Retained<objc2::runtime::AnyObject>,
    host: objc2::rc::Retained<objc2::runtime::AnyObject>,
}

#[cfg(target_os = "macos")]
impl VideoLayer {
    pub fn attach(window: &Window) -> Option<Self> {
        use objc2::msg_send;
        use objc2::rc::Retained;
        use objc2::runtime::{AnyClass, AnyObject};
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(appkit) = handle.as_raw() else { return None };
        let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
        // SAFETY: the window's live NSView; AVSampleBufferDisplayLayer is a CALayer subclass from
        // AVFoundation, which ym-mse links.
        unsafe {
            let host: Option<Retained<AnyObject>> = msg_send![view, layer];
            let host = host?;
            let class = AnyClass::get(c"AVSampleBufferDisplayLayer")?;
            let layer: Retained<AnyObject> = msg_send![class, new];
            let _: () = msg_send![&*layer, setHidden: true];
            let _: () = msg_send![&*host, addSublayer: &*layer];
            Some(Self { layer, host })
        }
    }

    pub fn as_ptr(&self) -> *mut c_void {
        objc2::rc::Retained::as_ptr(&self.layer) as *mut c_void
    }

    /// Window-relative logical bounds (top-left origin); hidden when `visible` is false.
    pub fn place(&self, bounds: Bounds<Pixels>, visible: bool) {
        use objc2::msg_send;
        use objc2::runtime::AnyClass;
        use objc2_core_foundation::{CGPoint, CGRect, CGSize};

        let Some(transaction) = AnyClass::get(c"CATransaction") else { return };
        // SAFETY: plain CALayer geometry calls on the main thread, inside a transaction with
        // implicit animations off.
        unsafe {
            let _: () = msg_send![transaction, begin];
            let _: () = msg_send![transaction, setDisableActions: true];
            let host_bounds: CGRect = msg_send![&*self.host, bounds];
            let flipped: bool = msg_send![&*self.host, isGeometryFlipped];
            let (x, y, width, height) = (
                f64::from(f32::from(bounds.origin.x)),
                f64::from(f32::from(bounds.origin.y)),
                f64::from(f32::from(bounds.size.width)),
                f64::from(f32::from(bounds.size.height)),
            );
            let y = if flipped { y } else { host_bounds.size.height - y - height };
            let frame = CGRect { origin: CGPoint { x, y }, size: CGSize { width, height } };
            let _: () = msg_send![&*self.layer, setFrame: frame];
            let _: () = msg_send![&*self.layer, setHidden: !visible];
            let _: () = msg_send![transaction, commit];
        }
    }

    pub fn hide(&self) {
        use objc2::msg_send;
        // SAFETY: a CALayer property on the main thread.
        unsafe {
            let _: () = msg_send![&*self.layer, setHidden: true];
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for VideoLayer {
    fn drop(&mut self) {
        use objc2::msg_send;
        // SAFETY: detaching our own sublayer.
        unsafe {
            let _: () = msg_send![&*self.layer, removeFromSuperlayer];
        }
    }
}
