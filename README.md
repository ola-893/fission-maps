# fission-maps

`fission-maps` provides a native MapKit widget for **macOS and iOS only**.

Its widget, actions, and reducer helpers depend on normal published Fission
`0.9.0` semver requirements (currently resolving to Fission 0.9.1). No
monorepo path or Git dependency is used.

## iOS native module

Add this to the application `fission.toml`:

```toml
[[native.modules]]
name = "fission-maps"

[native.modules.ios]
linked_frameworks = ["MapKit.framework", "CoreLocation.framework"]
```

macOS has no equivalent generic framework-link field in Fission's native
module configuration. The eventual MapKit adapter links `MapKit` and
`CoreLocation` with Rust framework link attributes and must be verified with a
real macOS build.

## Usage

```rust,ignore
use fission_core::{Widget, WidgetId};
use fission_maps::{Map, MapControlCtx};

let map = Map {
    id: Some(WidgetId::explicit("office-map")),
    center: (6.5244, 3.3792),
    zoom: 12.0,
    width: Some(360.0),
    height: Some(240.0),
    ..Default::default()
};

let controls: MapControlCtx = map.controls();
let move_to_london = controls.set_center(51.5072, -0.1276);
let map_widget: Widget = map.into();
```

Keep a `MapStateStore` in application state, implement `HasMapState`, and call
`register_map_reducers(&mut build_ctx)` once. The native handler will clone the
same store so `MapSetCenter` and `MapSetZoom` update MapKit without reintroducing
map state to Fission core.

## Native-surface handler

`MapKitSurfaceHandler` owns only the `fission-maps` custom payload. On macOS it
parents `MKMapView` inside the AppKit content view; on iOS it uses the UIKit
content view. It creates and detaches views as the Fission layout changes,
updates their frames, and links both MapKit and CoreLocation on macOS.

Enable the `native-surface-hook` feature and register one handler with the
application's `MapStateStore` using the shell's
`with_native_surface_handler` builder method.

## Release dependency

Published `fission-shell` 0.9.1 still has no generic
`NativeSurfaceHandler`/`NativeSurfaceFrame` API. The handler is implemented and
is compile-tested against the upstream hook, but cannot be enabled by a
published `fission-maps` release until that hook is merged and released. Using a
local Fission path or Git pin to bypass that condition would violate this
crate's published-dependency contract, so this repository intentionally does
not do so.
