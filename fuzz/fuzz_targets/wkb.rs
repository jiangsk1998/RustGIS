//! Well-known binary geometry, as GeoPackage, GeoParquet and PostGIS store it.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = rustgis_data::wkb::decode(data);
});
