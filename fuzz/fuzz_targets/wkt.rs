//! Coordinate system definitions as WKT: `.prj` files and GeoPackage spatial reference rows.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rustgis_data::raster::wkt::read_wkt;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = read_wkt(text);
    }
});
