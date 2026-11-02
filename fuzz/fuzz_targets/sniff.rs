//! A file dropped without an extension: its type is sniffed from its bytes, then it is read as the
//! app reads a file of that type, a raster (GeoTIFF, COG) by the raster reader and anything else
//! by `read_layers`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rustgis_data::io::{read_layers, sniff_extension};
use rustgis_data::raster;

fuzz_target!(|data: &[u8]| {
    if let Some(ext) = sniff_extension(data) {
        let name = format!("fuzz.{ext}");
        if raster::is_raster_name(&name) {
            let _ = raster::read_bytes(&name, data);
        } else {
            let _ = read_layers(&name, data);
        }
    }
});
