#![allow(unexpected_cfgs)]

//! Apple MapKit implementation for the generic Fission native-surface hook.
//!
//! This module deliberately belongs to `fission-maps`, not to a Fission shell:
//! the shell only routes opaque `EmbedKind::Custom` payloads.

use crate::{decode_payload, is_map_payload, MapRuntimeState, MapStateStore};
use fission_ir::WidgetId;
use fission_shell::{NativeSurfaceFrame, NativeSurfaceHandler, NativeSurfaceHost};
use raw_window_handle::RawWindowHandle;

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use cocoa::appkit::NSWindowOrderingMode;
    use cocoa::base::{id, nil, NO, YES};
    use core_graphics::geometry::{CGPoint, CGRect, CGSize};
    use fission_render::LayoutRect;
    use objc::rc::StrongPtr;
    use objc::{class, msg_send, sel, sel_impl};
    use std::collections::{HashMap, HashSet};

    #[link(name = "MapKit", kind = "framework")]
    extern "C" {}
    #[link(name = "CoreLocation", kind = "framework")]
    extern "C" {}

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CLLocationCoordinate2D {
        latitude: f64,
        longitude: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MKCoordinateSpan {
        latitude_delta: f64,
        longitude_delta: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MKCoordinateRegion {
        center: CLLocationCoordinate2D,
        span: MKCoordinateSpan,
    }

    #[derive(Clone)]
    struct RetainedId(StrongPtr);

    impl RetainedId {
        unsafe fn retain(ptr: id) -> Self {
            Self(StrongPtr::retain(ptr))
        }

        unsafe fn owned(ptr: id) -> Self {
            Self(StrongPtr::new(ptr))
        }

        fn as_id(&self) -> id {
            *self.0
        }
    }

    struct LayerContext {
        parent_view: id,
        bounds_height: f64,
    }

    /// A MapKit handler for AppKit-backed Fission windows.
    pub struct MapKitSurfaceHandler {
        state: MapStateStore,
        host_view: Option<RetainedId>,
        layers: HashMap<WidgetId, MapLayer>,
    }

    impl MapKitSurfaceHandler {
        pub fn new(state: MapStateStore) -> Self {
            Self {
                state,
                host_view: None,
                layers: HashMap::new(),
            }
        }

        fn detach_all(&mut self) {
            for layer in self.layers.values() {
                unsafe { layer.detach() };
            }
            self.layers.clear();
        }

        fn context(&self) -> Option<LayerContext> {
            unsafe {
                let view = self.host_view.as_ref()?.as_id();
                let wants_layer: bool = msg_send![view, wantsLayer];
                if !wants_layer {
                    let (): () = msg_send![view, setWantsLayer: YES];
                }
                let mut layer: id = msg_send![view, layer];
                if layer == nil {
                    layer = msg_send![class!(CALayer), layer];
                    let (): () = msg_send![view, setLayer: layer];
                }
                let window: id = msg_send![view, window];
                let scale: f64 = if window == nil {
                    1.0
                } else {
                    msg_send![window, backingScaleFactor]
                };
                let (): () = msg_send![layer, setContentsScale: scale];
                let bounds: CGRect = msg_send![view, bounds];
                Some(LayerContext {
                    parent_view: view,
                    bounds_height: bounds.size.height,
                })
            }
        }
    }

    impl NativeSurfaceHandler for MapKitSurfaceHandler {
        fn handles_payload(&self, payload: &[u8]) -> bool {
            is_map_payload(payload)
        }

        fn attach_host(&mut self, host: NativeSurfaceHost) {
            self.detach_all();
            self.host_view = match host.raw_window_handle() {
                RawWindowHandle::AppKit(handle) => {
                    Some(unsafe { RetainedId::retain(handle.ns_view.as_ptr() as id) })
                }
                _ => None,
            };
        }

        fn present_surfaces(&mut self, frames: &[NativeSurfaceFrame]) {
            if frames.is_empty() {
                self.detach_all();
                return;
            }
            let Some(context) = self.context() else {
                self.detach_all();
                return;
            };

            let mut seen = HashSet::new();
            for frame in frames {
                let Some(payload) = decode_payload(&frame.payload) else {
                    continue;
                };
                let state = self.state.ensure(frame.widget_id, payload.initial_state());
                let layer = self
                    .layers
                    .entry(frame.widget_id)
                    .or_insert_with(|| MapLayer::new(&context, &state));
                layer.update(&context, frame.rect, &state);
                seen.insert(frame.widget_id);
            }
            self.layers.retain(|id, layer| {
                if seen.contains(id) {
                    true
                } else {
                    unsafe { layer.detach() };
                    false
                }
            });
        }
    }

    impl Drop for MapKitSurfaceHandler {
        fn drop(&mut self) {
            self.detach_all();
        }
    }

    struct MapLayer {
        host_view: RetainedId,
        map_view: RetainedId,
        state: MapRuntimeState,
    }

    impl MapLayer {
        fn new(context: &LayerContext, state: &MapRuntimeState) -> Self {
            unsafe {
                let frame = CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(1.0, 1.0));
                let map_alloc: id = msg_send![class!(MKMapView), alloc];
                let map_view = RetainedId::owned(msg_send![map_alloc, initWithFrame: frame]);
                apply_state(map_view.as_id(), state);

                let container_alloc: id = msg_send![class!(NSView), alloc];
                let container = RetainedId::owned(msg_send![container_alloc, initWithFrame: frame]);
                let (): () = msg_send![container.as_id(), setWantsLayer: YES];
                let (): () = msg_send![container.as_id(), addSubview: map_view.as_id()];
                let (): () = msg_send![
                    context.parent_view,
                    addSubview: container.as_id()
                    positioned: NSWindowOrderingMode::NSWindowAbove
                    relativeTo: nil
                ];
                Self {
                    host_view: container,
                    map_view,
                    state: state.clone(),
                }
            }
        }

        fn update(&mut self, context: &LayerContext, rect: LayoutRect, state: &MapRuntimeState) {
            unsafe {
                let frame = layout_rect(rect, context);
                let (): () = msg_send![self.host_view.as_id(), setFrame: frame];
                let bounds: CGRect = msg_send![self.host_view.as_id(), bounds];
                let (): () = msg_send![self.map_view.as_id(), setFrame: bounds];
                let (): () = msg_send![self.host_view.as_id(), addSubview: self.map_view.as_id()];
                let (): () = msg_send![
                    context.parent_view,
                    addSubview: self.host_view.as_id()
                    positioned: NSWindowOrderingMode::NSWindowAbove
                    relativeTo: nil
                ];
                if self.state != *state {
                    apply_state(self.map_view.as_id(), state);
                    self.state = state.clone();
                }
            }
        }

        unsafe fn detach(&self) {
            let (): () = msg_send![self.map_view.as_id(), removeFromSuperview];
            let (): () = msg_send![self.host_view.as_id(), removeFromSuperview];
        }
    }

    fn layout_rect(rect: LayoutRect, context: &LayerContext) -> CGRect {
        CGRect::new(
            &CGPoint::new(
                rect.origin.x as f64,
                context.bounds_height - rect.origin.y as f64 - rect.size.height as f64,
            ),
            &CGSize::new(rect.size.width as f64, rect.size.height as f64),
        )
    }

    fn apply_state(map_view: id, state: &MapRuntimeState) {
        unsafe {
            let coordinate = CLLocationCoordinate2D {
                latitude: state.center.0,
                longitude: state.center.1,
            };
            let delta = 360.0 / 2.0_f64.powf(state.zoom.clamp(0.0, 22.0) as f64);
            let region = MKCoordinateRegion {
                center: coordinate,
                span: MKCoordinateSpan {
                    latitude_delta: delta,
                    longitude_delta: delta,
                },
            };
            let (): () = msg_send![map_view, setRegion: region animated: NO];
            let user_location = if state.show_user_location { YES } else { NO };
            let interactive = if state.interactive { YES } else { NO };
            let (): () = msg_send![map_view, setShowsUserLocation: user_location];
            let (): () = msg_send![map_view, setScrollEnabled: interactive];
            let (): () = msg_send![map_view, setZoomEnabled: interactive];
            let (): () = msg_send![map_view, setRotateEnabled: interactive];
            let (): () = msg_send![map_view, setPitchEnabled: interactive];
        }
    }
}

#[cfg(target_os = "ios")]
mod platform {
    use super::*;
    use core_graphics::geometry::{CGPoint, CGRect, CGSize};
    use fission_render::LayoutRect;
    use objc::rc::StrongPtr;
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    use std::collections::{HashMap, HashSet};

    type Id = *mut Object;
    const YES: i8 = 1;
    const NO: i8 = 0;

    #[link(name = "MapKit", kind = "framework")]
    extern "C" {}
    #[link(name = "CoreLocation", kind = "framework")]
    extern "C" {}

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CLLocationCoordinate2D {
        latitude: f64,
        longitude: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MKCoordinateSpan {
        latitude_delta: f64,
        longitude_delta: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MKCoordinateRegion {
        center: CLLocationCoordinate2D,
        span: MKCoordinateSpan,
    }

    #[derive(Clone)]
    struct RetainedId(StrongPtr);

    impl RetainedId {
        unsafe fn retain(ptr: Id) -> Self {
            Self(StrongPtr::retain(ptr))
        }
        unsafe fn owned(ptr: Id) -> Self {
            Self(StrongPtr::new(ptr))
        }
        fn as_id(&self) -> Id {
            *self.0
        }
    }

    /// A MapKit handler for UIKit-backed Fission windows.
    pub struct MapKitSurfaceHandler {
        state: MapStateStore,
        host_view: Option<RetainedId>,
        layers: HashMap<WidgetId, MapLayer>,
    }

    impl MapKitSurfaceHandler {
        pub fn new(state: MapStateStore) -> Self {
            Self {
                state,
                host_view: None,
                layers: HashMap::new(),
            }
        }

        fn detach_all(&mut self) {
            for layer in self.layers.values() {
                unsafe { layer.detach() };
            }
            self.layers.clear();
        }
    }

    impl NativeSurfaceHandler for MapKitSurfaceHandler {
        fn handles_payload(&self, payload: &[u8]) -> bool {
            is_map_payload(payload)
        }

        fn attach_host(&mut self, host: NativeSurfaceHost) {
            self.detach_all();
            self.host_view = match host.raw_window_handle() {
                RawWindowHandle::UiKit(handle) => {
                    Some(unsafe { RetainedId::retain(handle.ui_view.as_ptr() as Id) })
                }
                _ => None,
            };
        }

        fn present_surfaces(&mut self, frames: &[NativeSurfaceFrame]) {
            if frames.is_empty() {
                self.detach_all();
                return;
            }
            let Some(parent) = self.host_view.as_ref().map(RetainedId::as_id) else {
                self.detach_all();
                return;
            };

            let mut seen = HashSet::new();
            for frame in frames {
                let Some(payload) = decode_payload(&frame.payload) else {
                    continue;
                };
                let state = self.state.ensure(frame.widget_id, payload.initial_state());
                let layer = self
                    .layers
                    .entry(frame.widget_id)
                    .or_insert_with(|| MapLayer::new(parent, &state));
                layer.update(parent, frame.rect, &state);
                seen.insert(frame.widget_id);
            }
            self.layers.retain(|id, layer| {
                if seen.contains(id) {
                    true
                } else {
                    unsafe { layer.detach() };
                    false
                }
            });
        }
    }

    impl Drop for MapKitSurfaceHandler {
        fn drop(&mut self) {
            self.detach_all();
        }
    }

    struct MapLayer {
        host_view: RetainedId,
        map_view: RetainedId,
        state: MapRuntimeState,
    }

    impl MapLayer {
        fn new(parent: Id, state: &MapRuntimeState) -> Self {
            unsafe {
                let frame = CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(1.0, 1.0));
                let map_alloc: Id = msg_send![class!(MKMapView), alloc];
                let map_view = RetainedId::owned(msg_send![map_alloc, initWithFrame: frame]);
                apply_state(map_view.as_id(), state);
                let container_alloc: Id = msg_send![class!(UIView), alloc];
                let container = RetainedId::owned(msg_send![container_alloc, initWithFrame: frame]);
                let (): () = msg_send![container.as_id(), addSubview: map_view.as_id()];
                let (): () = msg_send![parent, addSubview: container.as_id()];
                Self {
                    host_view: container,
                    map_view,
                    state: state.clone(),
                }
            }
        }

        fn update(&mut self, parent: Id, rect: LayoutRect, state: &MapRuntimeState) {
            unsafe {
                let frame = CGRect::new(
                    &CGPoint::new(rect.origin.x as f64, rect.origin.y as f64),
                    &CGSize::new(rect.size.width as f64, rect.size.height as f64),
                );
                let (): () = msg_send![self.host_view.as_id(), setFrame: frame];
                let bounds: CGRect = msg_send![self.host_view.as_id(), bounds];
                let (): () = msg_send![self.map_view.as_id(), setFrame: bounds];
                let (): () = msg_send![self.host_view.as_id(), addSubview: self.map_view.as_id()];
                let (): () = msg_send![parent, addSubview: self.host_view.as_id()];
                if self.state != *state {
                    apply_state(self.map_view.as_id(), state);
                    self.state = state.clone();
                }
            }
        }

        unsafe fn detach(&self) {
            let (): () = msg_send![self.map_view.as_id(), removeFromSuperview];
            let (): () = msg_send![self.host_view.as_id(), removeFromSuperview];
        }
    }

    fn apply_state(map_view: Id, state: &MapRuntimeState) {
        unsafe {
            let coordinate = CLLocationCoordinate2D {
                latitude: state.center.0,
                longitude: state.center.1,
            };
            let delta = 360.0 / 2.0_f64.powf(state.zoom.clamp(0.0, 22.0) as f64);
            let region = MKCoordinateRegion {
                center: coordinate,
                span: MKCoordinateSpan {
                    latitude_delta: delta,
                    longitude_delta: delta,
                },
            };
            let (): () = msg_send![map_view, setRegion: region animated: NO];
            let user_location = if state.show_user_location { YES } else { NO };
            let interactive = if state.interactive { YES } else { NO };
            let (): () = msg_send![map_view, setShowsUserLocation: user_location];
            let (): () = msg_send![map_view, setScrollEnabled: interactive];
            let (): () = msg_send![map_view, setZoomEnabled: interactive];
            let (): () = msg_send![map_view, setRotateEnabled: interactive];
            let (): () = msg_send![map_view, setPitchEnabled: interactive];
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use platform::MapKitSurfaceHandler;

#[cfg(all(test, any(target_os = "macos", target_os = "ios")))]
mod tests {
    use super::*;
    use crate::{encode_payload, Map};

    #[test]
    fn only_claims_valid_fission_maps_payloads() {
        let handler = MapKitSurfaceHandler::new(MapStateStore::default());
        assert!(handler.handles_payload(&encode_payload(&Map::default())));
        assert!(!handler.handles_payload(b"fission-maps\0v1not-bincode"));
        assert!(!handler.handles_payload(b"other-extension"));
    }
}
