#![allow(unexpected_cfgs)] // `objc` 0.2 probes its removed `cargo-clippy` feature.

//! Native MapKit maps for Fission applications on macOS and iOS.
//!
//! The core widget and action APIs use the published Fission 0.9.0 crates.

use fission_core::internal::{
    custom_render_widget, BuildCtx, InternalIrBuilder, InternalLowerer, InternalLoweringCx,
    InternalRenderNode,
};
use fission_core::{
    Action, ActionEnvelope, ActionId, EmbedKind, GlobalState, LayoutOp, Op, Widget,
};
use fission_ir::WidgetId;
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const MAP_PAYLOAD_MAGIC: &[u8] = b"fission-maps\0v1";

#[cfg(all(
    feature = "native-surface-hook",
    any(target_os = "macos", target_os = "ios")
))]
mod mapkit_handler;

/// The Apple MapKit implementation of Fission's native-surface extension.
///
/// Register this with `WinitApp::with_native_surface_handler` (or the desktop
/// or mobile wrapper) after the native-surface hook is released by Fission.
#[cfg(all(
    feature = "native-surface-hook",
    any(target_os = "macos", target_os = "ios")
))]
pub use mapkit_handler::MapKitSurfaceHandler;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapSetCenter {
    pub target: WidgetId,
    pub latitude: f64,
    pub longitude: f64,
}

impl Action for MapSetCenter {
    fn static_id() -> ActionId {
        *MAP_SET_CENTER_ID
    }
}

lazy_static! {
    static ref MAP_SET_CENTER_ID: ActionId = ActionId::from_name("fission_maps::MapSetCenter");
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapSetZoom {
    pub target: WidgetId,
    pub zoom: f32,
}

impl Action for MapSetZoom {
    fn static_id() -> ActionId {
        *MAP_SET_ZOOM_ID
    }
}

lazy_static! {
    static ref MAP_SET_ZOOM_ID: ActionId = ActionId::from_name("fission_maps::MapSetZoom");
}

/// Builds map control action envelopes for one map widget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapControlCtx {
    target: WidgetId,
}

impl MapControlCtx {
    pub const fn new(target: WidgetId) -> Self {
        Self { target }
    }

    pub fn set_center(self, latitude: f64, longitude: f64) -> ActionEnvelope {
        let action = MapSetCenter {
            target: self.target,
            latitude,
            longitude,
        };
        ActionEnvelope {
            id: MapSetCenter::static_id(),
            payload: action.encode(),
        }
    }

    pub fn set_zoom(self, zoom: f32) -> ActionEnvelope {
        let action = MapSetZoom {
            target: self.target,
            zoom,
        };
        ActionEnvelope {
            id: MapSetZoom::static_id(),
            payload: action.encode(),
        }
    }
}

/// State shared by application reducers and the native MapKit handler.
#[derive(Clone, Debug, Default)]
pub struct MapStateStore {
    states: Arc<Mutex<HashMap<WidgetId, MapRuntimeState>>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MapRuntimeState {
    pub center: (f64, f64),
    pub zoom: f32,
    pub show_user_location: bool,
    pub interactive: bool,
}

impl Default for MapRuntimeState {
    fn default() -> Self {
        Self {
            center: (0.0, 0.0),
            zoom: 10.0,
            show_user_location: false,
            interactive: true,
        }
    }
}

impl MapStateStore {
    pub fn ensure(&self, id: WidgetId, initial: MapRuntimeState) -> MapRuntimeState {
        self.states
            .lock()
            .expect("map state lock poisoned")
            .entry(id)
            .or_insert(initial)
            .clone()
    }

    pub fn get(&self, id: WidgetId) -> Option<MapRuntimeState> {
        self.states
            .lock()
            .expect("map state lock poisoned")
            .get(&id)
            .cloned()
    }

    pub fn set_center(&self, id: WidgetId, latitude: f64, longitude: f64) {
        self.states
            .lock()
            .expect("map state lock poisoned")
            .entry(id)
            .or_default()
            .center = (latitude, longitude);
    }

    pub fn set_zoom(&self, id: WidgetId, zoom: f32) {
        self.states
            .lock()
            .expect("map state lock poisoned")
            .entry(id)
            .or_default()
            .zoom = zoom;
    }
}

pub trait HasMapState {
    fn map_state(&self) -> &MapStateStore;
}

pub fn register_map_reducers<S>(ctx: &mut BuildCtx<S>)
where
    S: GlobalState + HasMapState,
{
    ctx.register(reduce_center::<S> as fn(&mut S, MapSetCenter));
    ctx.register(reduce_zoom::<S> as fn(&mut S, MapSetZoom));
}

fn reduce_center<S: HasMapState>(state: &mut S, action: MapSetCenter) {
    state
        .map_state()
        .set_center(action.target, action.latitude, action.longitude);
}

fn reduce_zoom<S: HasMapState>(state: &mut S, action: MapSetZoom) {
    state.map_state().set_zoom(action.target, action.zoom);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Map {
    pub id: Option<WidgetId>,
    pub center: (f64, f64),
    pub zoom: f32,
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub show_user_location: bool,
    pub interactive: bool,
}

impl Default for Map {
    fn default() -> Self {
        Self {
            id: None,
            center: (0.0, 0.0),
            zoom: 10.0,
            width: None,
            height: None,
            show_user_location: false,
            interactive: true,
        }
    }
}

impl Map {
    pub fn widget_id(&self) -> WidgetId {
        self.id.unwrap_or_else(|| {
            WidgetId::explicit(&format!("fission-maps:{}:{}", self.center.0, self.center.1))
        })
    }

    pub fn controls(&self) -> MapControlCtx {
        MapControlCtx::new(self.widget_id())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MapPayload {
    center: (f64, f64),
    zoom: f32,
    show_user_location: bool,
    interactive: bool,
}

#[cfg(any(test, feature = "native-surface-hook"))]
impl MapPayload {
    fn initial_state(&self) -> MapRuntimeState {
        MapRuntimeState {
            center: self.center,
            zoom: self.zoom,
            show_user_location: self.show_user_location,
            interactive: self.interactive,
        }
    }
}

fn encode_payload(map: &Map) -> Vec<u8> {
    let payload = MapPayload {
        center: map.center,
        zoom: map.zoom,
        show_user_location: map.show_user_location,
        interactive: map.interactive,
    };
    let mut bytes = MAP_PAYLOAD_MAGIC.to_vec();
    bytes.extend(bincode::serialize(&payload).expect("Map payload serialization is infallible"));
    bytes
}

#[cfg(any(test, feature = "native-surface-hook"))]
fn decode_payload(payload: &[u8]) -> Option<MapPayload> {
    payload
        .strip_prefix(MAP_PAYLOAD_MAGIC)
        .and_then(|payload| bincode::deserialize(payload).ok())
}

#[cfg(any(test, feature = "native-surface-hook"))]
fn is_map_payload(payload: &[u8]) -> bool {
    decode_payload(payload).is_some()
}

#[derive(Debug)]
struct MapLowerer(Map);

impl InternalLowerer for MapLowerer {
    fn lower_dyn(&self, cx: &mut InternalLoweringCx) -> WidgetId {
        let map = &self.0;
        let id = map.widget_id();
        let embed = InternalIrBuilder::new(
            cx.next_node_id(),
            Op::Layout(LayoutOp::Embed {
                kind: EmbedKind::Custom(encode_payload(map)),
                widget_id: id,
                width: map.width,
                height: map.height,
            }),
        )
        .build(cx);
        let mut layout = InternalIrBuilder::new(
            cx.widget_node_id(id),
            Op::Layout(LayoutOp::Box {
                width: map.width,
                height: map.height,
                min_width: None,
                max_width: None,
                min_height: None,
                max_height: None,
                padding: [0.0; 4],
                flex_grow: 0.0,
                flex_shrink: 0.0,
                aspect_ratio: None,
            }),
        );
        layout.add_child(embed);
        layout.build(cx)
    }
}

impl From<Map> for Widget {
    fn from(map: Map) -> Self {
        custom_render_widget(InternalRenderNode {
            debug_tag: "fission-maps::Map".into(),
            lowerer: Some(Arc::new(MapLowerer(map))),
            render_object: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fission_core::internal::lower_widget_to_ir;

    #[derive(Debug, Default)]
    struct TestState {
        maps: MapStateStore,
    }
    impl GlobalState for TestState {}
    impl HasMapState for TestState {
        fn map_state(&self) -> &MapStateStore {
            &self.maps
        }
    }

    #[test]
    fn reducers_update_map_state() {
        let mut state = TestState::default();
        let id = WidgetId::explicit("map.a");
        reduce_center(
            &mut state,
            MapSetCenter {
                target: id,
                latitude: 37.7749,
                longitude: -122.4194,
            },
        );
        reduce_zoom(
            &mut state,
            MapSetZoom {
                target: id,
                zoom: 14.0,
            },
        );
        assert_eq!(state.maps.get(id).unwrap().center, (37.7749, -122.4194));
        assert_eq!(state.maps.get(id).unwrap().zoom, 14.0);
    }

    #[test]
    fn map_lowers_to_a_custom_embed() {
        let map = Map {
            id: Some(WidgetId::explicit("map.a")),
            width: Some(320.0),
            height: Some(180.0),
            ..Default::default()
        };
        let ir = lower_widget_to_ir(&Widget::from(map));
        assert!(ir.nodes.values().any(|node| matches!(
            &node.op,
            Op::Layout(LayoutOp::Embed { kind: EmbedKind::Custom(payload), .. }) if payload.starts_with(MAP_PAYLOAD_MAGIC)
        )));
    }

    #[test]
    fn map_payload_has_a_stable_type_marker_and_round_trips() {
        let map = Map {
            center: (6.5244, 3.3792),
            zoom: 12.0,
            show_user_location: true,
            interactive: false,
            ..Default::default()
        };
        let payload = encode_payload(&map);
        assert!(is_map_payload(&payload));
        assert_eq!(
            decode_payload(&payload).unwrap().initial_state(),
            MapRuntimeState {
                center: map.center,
                zoom: map.zoom,
                show_user_location: map.show_user_location,
                interactive: map.interactive,
            }
        );
    }
}
