//! Terrain pipeline for the VR wildfire simulator: USGS 3DEP → EPSG:5070 height tiles → glTF meshes.

#[cfg(feature = "pipeline")]
pub mod bake;
pub mod crs;
pub mod dem;
#[cfg(feature = "pipeline")]
pub mod export;
pub mod grid;
#[cfg(feature = "pipeline")]
pub mod ingest;
pub mod mesh;
pub mod region;
#[cfg(feature = "pipeline")]
pub mod reproject;
#[cfg(feature = "pipeline")]
pub mod source;
#[cfg(feature = "pipeline")]
pub mod store;
