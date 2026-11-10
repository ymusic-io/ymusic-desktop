//! The surface YouTube video presents into: an `AVSampleBufferDisplayLayer` hosted by a native
//! view placed BEHIND the gpui view, inside the transparent window. gpui leaves the video box
//! unpainted (a hole), so its controls, captions and dialogs draw over the video.

use std::ffi::c_void;

use gpui_kit::{Bounds, Pixels, Window};

#[cfg(target_os = "macos")]
#[repr(C)]
struct CGColor {
    _opaque: [u8; 0],
}

// SAFETY: matches the `^{CGColor=}` type AppKit declares for `-[NSColor CGColor]`.
#[cfg(target_os = "macos")]
unsafe impl objc2::RefEncode for CGColor {
    const ENCODING_REF: objc2::Encoding = objc2::Encoding::Pointer(&objc2::Encoding::Struct("CGColor", &[]));
}

#[cfg(target_os = "macos")]
pub struct VideoLayer {
    layer: objc2::rc::Retained<objc2::runtime::AnyObject>,
    view: objc2::rc::Retained<objc2::runtime::AnyObject>,
    /// gpui's view, whose coordinates the box arrives in.
    gpui_view: objc2::rc::Retained<objc2::runtime::AnyObject>,
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
        let gpui_view = appkit.ns_view.as_ptr().cast::<AnyObject>();
        // SAFETY: gpui's live NSView, a subview of the window's content view; NSView and
        // AVSampleBufferDisplayLayer (AVFoundation, linked by ym-mse) are used on the main thread.
        unsafe {
            let gpui_view: Retained<AnyObject> = Retained::retain(gpui_view)?;
            let superview: Option<Retained<AnyObject>> = msg_send![&*gpui_view, superview];
            let superview = superview?;
            let layer_class = AnyClass::get(c"AVSampleBufferDisplayLayer")?;
            let layer: Retained<AnyObject> = msg_send![layer_class, new];
            let color_class = AnyClass::get(c"NSColor")?;
            let black: Retained<AnyObject> = msg_send![color_class, blackColor];
            let cg_black: *const CGColor = msg_send![&*black, CGColor];
            let _: () = msg_send![&*layer, setBackgroundColor: cg_black];
            let view_class = AnyClass::get(c"NSView")?;
            let view: Retained<AnyObject> = msg_send![view_class, new];
            // A layer-hosting view: set the layer, then ask for it.
            let _: () = msg_send![&*view, setLayer: &*layer];
            let _: () = msg_send![&*view, setWantsLayer: true];
            let _: () = msg_send![&*view, setHidden: true];
            // NSWindowBelow = -1: under gpui's view.
            let _: () = msg_send![&*superview, addSubview: &*view, positioned: -1isize, relativeTo: &*gpui_view];
            Some(Self { layer, view, gpui_view })
        }
    }

    pub fn as_ptr(&self) -> *mut c_void {
        objc2::rc::Retained::as_ptr(&self.layer) as *mut c_void
    }

    /// Window-relative logical bounds (top-left origin); hidden when `visible` is false.
    pub fn place(&self, bounds: Bounds<Pixels>, visible: bool) {
        use objc2::msg_send;
        use objc2::rc::Retained;
        use objc2::runtime::AnyObject;
        use objc2_core_foundation::{CGPoint, CGRect, CGSize};

        // SAFETY: NSView geometry on the main thread.
        unsafe {
            let superview: Option<Retained<AnyObject>> = msg_send![&*self.gpui_view, superview];
            let Some(superview) = superview else { return };
            let gpui_bounds: CGRect = msg_send![&*self.gpui_view, bounds];
            let flipped: bool = msg_send![&*self.gpui_view, isFlipped];
            let (x, y, width, height) = (
                f64::from(f32::from(bounds.origin.x)),
                f64::from(f32::from(bounds.origin.y)),
                f64::from(f32::from(bounds.size.width)),
                f64::from(f32::from(bounds.size.height)),
            );
            let y = if flipped { y } else { gpui_bounds.size.height - y - height };
            let local = CGRect { origin: CGPoint { x, y }, size: CGSize { width, height } };
            let frame: CGRect = msg_send![&*self.gpui_view, convertRect: local, toView: &*superview];
            let _: () = msg_send![&*self.view, setFrame: frame];
            let _: () = msg_send![&*self.view, setHidden: !visible];
        }
    }

    pub fn hide(&self) {
        use objc2::msg_send;
        // SAFETY: an NSView property on the main thread.
        unsafe {
            let _: () = msg_send![&*self.view, setHidden: true];
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for VideoLayer {
    fn drop(&mut self) {
        use objc2::msg_send;
        // SAFETY: detaching our own view.
        unsafe {
            let _: () = msg_send![&*self.view, removeFromSuperview];
        }
    }
}
