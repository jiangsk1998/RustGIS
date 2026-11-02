//! Mapbox Vector Tiles, from any tile server or archive.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = rustgis_tiles::mvt::decode(data);
});
