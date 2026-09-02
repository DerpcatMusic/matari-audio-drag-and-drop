use std::path::PathBuf;
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBitmapImageRep, NSDeviceRGBColorSpace, NSDragOperation, NSDraggingContext, NSDraggingItem,
    NSDraggingSession, NSDraggingSource, NSEvent, NSImage, NSImageRep, NSView,
};
use objc2_foundation::{
    NSArray, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL,
};
use raw_window_handle::RawWindowHandle;

use super::ExternalDragPayload;
use super::{DragWindow, ExternalDragError};
use crate::{DragPreview, Outcome, PreviewFailureStage, PreviewStatus, SessionReporter};

struct DragSourceIvars {
    reporter: Mutex<Option<SessionReporter>>,
    owns_self: AtomicBool,
}

define_class!(
    // SAFETY: The class has an `NSObject` superclass, uses ivars initialized by
    // `set_ivars`, and is confined to AppKit's main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "MatariExternalDragSource"]
    #[ivars = DragSourceIvars]
    struct MatariExternalDragSource;

    // SAFETY: `define_class!` registers this type as an `NSObject` subclass,
    // so every instance satisfies `NSObjectProtocol`.
    unsafe impl NSObjectProtocol for MatariExternalDragSource {}

    #[allow(non_snake_case)]
    // SAFETY: Both methods use the exact selectors and signatures required by
    // `NSDraggingSource`; `MainThreadOnly` enforces AppKit thread affinity.
    unsafe impl NSDraggingSource for MatariExternalDragSource {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn draggingSession_sourceOperationMaskForDraggingContext(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn draggingSession_endedAtPoint_operation(
            &self,
            _session: &NSDraggingSession,
            _screen_point: NSPoint,
            operation: NSDragOperation,
        ) {
            let reporter = self
                .ivars()
                .reporter
                .lock()
                .map(|mut reporter| reporter.take())
                .unwrap_or(None);
            if let Some(reporter) = reporter {
                let outcome = if operation.contains(NSDragOperation::Copy) {
                    Outcome::Copied
                } else {
                    Outcome::Cancelled
                };
                reporter.finish(outcome);
            }
            if self.ivars().owns_self.swap(false, Ordering::AcqRel) {
                let pointer = std::ptr::from_ref(self).cast_mut();
                // SAFETY: `start_drag_from_view` created exactly one retained
                // self-ownership with `Retained::into_raw`; this terminal
                // callback consumes that ownership exactly once.
                let retained: Option<Retained<Self>> = unsafe { Retained::from_raw(pointer) };
                drop(retained);
            }
        }
    }
);

impl MatariExternalDragSource {
    fn new(mtm: MainThreadMarker, reporter: Option<SessionReporter>) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(DragSourceIvars {
            reporter: Mutex::new(reporter),
            owns_self: AtomicBool::new(true),
        });
        // SAFETY: `this` is a newly allocated instance whose ivars were
        // initialized above; invoking the `NSObject` superclass initializer is
        // the required final construction step.
        unsafe { msg_send![super(this), init] }
    }
}

pub(super) fn start_external_file_drag(
    window: DragWindow<'_>,
    payload: ExternalDragPayload,
    appkit_event: Option<std::ptr::NonNull<std::ffi::c_void>>,
    reporter: Option<SessionReporter>,
) -> Result<(), ExternalDragError> {
    let ExternalDragPayload { paths, preview } = payload;

    if paths.is_empty() {
        return Err(ExternalDragError::EmptyPayload);
    }
    validate_paths(&paths)?;

    let ns_view = match window.window().as_raw() {
        RawWindowHandle::AppKit(handle) => handle.ns_view.as_ptr().cast(),
        other => {
            return Err(ExternalDragError::UnsupportedBackend {
                backend: window.backend_kind(),
                window: format!("{other:?}"),
            });
        }
    };

    let mtm = MainThreadMarker::new().ok_or_else(|| {
        "macOS external file drag must start on the AppKit main thread".to_string()
    })?;
    let event = appkit_event
        // SAFETY: `DragOrigin::with_appkit_event` requires this to be a live
        // `NSEvent` from the same main-thread gesture; `mtm` proves this call
        // is executing on that main thread.
        .map(|event| unsafe { &*event.cast::<NSEvent>().as_ptr() })
        .ok_or_else(|| {
            "macOS external file drag needs the exact initiating AppKit NSEvent".to_string()
        })?;

    // SAFETY: `RawWindowHandle::AppKit` guarantees `ns_view` identifies the
    // live `NSView` borrowed through `window`; `mtm` proves main-thread access.
    let view = unsafe { &*ns_view };
    start_drag_from_view(view, event, &paths, preview.as_ref(), reporter, mtm);
    Ok(())
}

fn start_drag_from_view(
    view: &NSView,
    event: &NSEvent,
    paths: &[PathBuf],
    preview: Option<&DragPreview>,
    reporter: Option<SessionReporter>,
    mtm: MainThreadMarker,
) {
    let location = event.locationInWindow();
    let image = preview.and_then(|preview| match ns_image_from_preview(preview) {
        Ok(image) => Some(image),
        Err(stage) => {
            if let Some(reporter) = &reporter {
                reporter.preview(PreviewStatus::Unavailable {
                    stage,
                    native_code: None,
                });
            }
            None
        }
    });
    let items = dragging_items(paths, location, image.as_ref());
    if image.is_some()
        && let Some(reporter) = &reporter
    {
        reporter.preview(PreviewStatus::Attached);
    }
    let item_refs = items.iter().map(|item| &**item).collect::<Vec<_>>();
    let item_array = NSArray::from_slice(&item_refs);
    let source = MatariExternalDragSource::new(mtm, reporter);
    let source_ref: &ProtocolObject<dyn NSDraggingSource> = ProtocolObject::from_ref(&*source);

    let _session = view.beginDraggingSessionWithItems_event_source(&item_array, event, source_ref);
    let _self_owned_until_terminal = Retained::into_raw(source);
}

fn dragging_items(
    paths: &[PathBuf],
    location: NSPoint,
    image: Option<&Retained<NSImage>>,
) -> Vec<Retained<NSDraggingItem>> {
    let width = crate::preview::WIDTH as f64;
    let height = crate::preview::HEIGHT as f64;
    paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let absolute = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            let path_string = NSString::from_str(&absolute.to_string_lossy());
            let file_url = NSURL::fileURLWithPath(&path_string);
            let writer: &ProtocolObject<dyn objc2_app_kit::NSPasteboardWriting> =
                ProtocolObject::from_ref(&*file_url);
            let dragging_item =
                NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), writer);
            let offset = index as f64 * 4.0;
            // SAFETY: `dragging_item` is fully initialized and the image, when
            // present, remains retained while AppKit copies the drag contents.
            unsafe {
                let contents = image.map(|image| {
                    let image: &NSImage = image;
                    image as &objc2::runtime::AnyObject
                });
                dragging_item.setDraggingFrame_contents(
                    NSRect::new(
                        NSPoint::new(
                            location.x - width * 0.5 + offset,
                            location.y - height * 0.5 - offset,
                        ),
                        NSSize::new(width, height),
                    ),
                    contents,
                );
            }
            dragging_item
        })
        .collect()
}

fn ns_image_from_preview(preview: &DragPreview) -> Result<Retained<NSImage>, PreviewFailureStage> {
    let pixels = crate::preview::render(preview);
    let width = crate::preview::WIDTH;
    let height = crate::preview::HEIGHT;
    let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(width as f64, height as f64));
    // SAFETY: The geometry and 8-bit RGBA layout describe the initialized
    // `pixels` buffer copied below.
    let Some(bitmap) = (unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            ptr::null_mut(),
            width as isize,
            height as isize,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            (width * 4) as isize,
            32,
        )
    }) else {
        return Err(PreviewFailureStage::Bitmap);
    };
    let destination = bitmap.bitmapData();
    if destination.is_null() {
        return Err(PreviewFailureStage::Bitmap);
    }
    // SAFETY: AppKit allocated at least `width * height * 4` bytes for the
    // bitmap layout above, and `pixels` has exactly that length.
    unsafe {
        ptr::copy_nonoverlapping(pixels.as_ptr(), destination, pixels.len());
    }
    let representation: &NSImageRep = bitmap.as_ref();
    image.addRepresentation(representation);
    Ok(image)
}

fn validate_paths(paths: &[PathBuf]) -> Result<(), String> {
    for path in paths {
        let metadata = std::fs::metadata(path)
            .map_err(|err| format!("drag file is not readable: {}: {err}", path.display()))?;
        if !metadata.is_file() {
            return Err(format!("drag path is not a file: {}", path.display()));
        }
        if metadata.len() == 0 {
            return Err(format!("drag file is empty: {}", path.display()));
        }
    }
    Ok(())
}
