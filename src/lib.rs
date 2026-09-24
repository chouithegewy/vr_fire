//! Terrain pipeline for the VR wildfire simulator: USGS 3DEP → EPSG:5070 height tiles → glTF meshes.

pub mod crs;
pub mod dem;
pub mod grid;
pub mod region;
pub mod reproject;
pub mod source;
pub mod store;
