//! GeoTIFF rasters, through RustGIS's reader and the `tiff` crate beneath it.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rustgis_data::raster::geotiff;

fuzz_target!(|data: &[u8]| {
    let _ = geotiff::read(std::io::Cursor::new(data), "fuzz.tif");
});
