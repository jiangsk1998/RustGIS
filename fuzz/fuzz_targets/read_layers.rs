//! Every reader behind `io::read_layers`: GeoJSON, CSV, KML/KMZ, GPX, GML, DXF, OSM PBF, TopoJSON,
//! MapInfo, zipped Shapefiles, GeoPackage, GeoParquet, FlatGeobuf, LAS/LAZ and the rest. The first
//! byte picks the file's extension from `READ_EXTENSIONS`; the rest is the file.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rustgis_data::io::{READ_EXTENSIONS, read_layers};

fuzz_target!(|data: &[u8]| {
    let Some((&pick, bytes)) = data.split_first() else { return };
    let ext = READ_EXTENSIONS[usize::from(pick) % READ_EXTENSIONS.len()];
    let _ = read_layers(&format!("fuzz.{ext}"), bytes);
});
