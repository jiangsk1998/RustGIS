//! Writes the seed corpus into `corpus/<target>/`: well-formed files made by RustGIS's own writers
//! from the sample project's layers, so fuzzing starts from real files rather than guessing each
//! format's magic bytes. The repository commits no data files; run this before fuzzing:
//!
//! ```sh
//! cargo run --release --bin seeds
//! ```

use rustgis_data::io::{READ_EXTENSIONS, write_bytes, write_path};
use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff, wkt};
use rustgis_data::{Dataset, wkb};
use rustgis_engine::{Project, Session};
use rustgis_tiles::mvt;
use std::path::{Path, PathBuf};

/// Features kept from each sample layer: seeds stay small, which libFuzzer mutates faster.
const FEATURES: usize = 12;
/// The formats RustGIS writes, as `write_bytes` names them, and the extension each is read back by.
const WRITTEN: &[(&str, &str)] = &[
    ("geojson", "geojson"),
    ("csv", "csv"),
    ("kml", "kml"),
    ("kmz", "kmz"),
    ("gpx", "gpx"),
    ("gpkg", "gpkg"),
    ("parquet", "parquet"),
    ("fgb", "fgb"),
    ("shp", "zip"),
];
/// Coordinate systems written as WKT seeds: geographic, Web Mercator, UTM, a national grid, a
/// conic and a polar system.
const EPSG: &[u32] = &[4326, 3857, 32616, 27700, 2263, 3413];

type Result<T> = std::result::Result<T, String>;

/// Writes one seed file, named by its target and a counter.
fn seed(dir: &Path, n: &mut usize, bytes: &[u8]) -> Result<()> {
    *n += 1;
    std::fs::write(dir.join(format!("seed-{n:03}")), bytes).map_err(|e| format!("{}: {e}", dir.display()))
}

/// A dataset written as `format`. GeoPackage is written to a file (an SQLite database), and read
/// back; every other format in `WRITTEN` comes straight from `write_bytes`.
fn written(ds: &Dataset, format: &str, scratch: &Path) -> Result<Vec<u8>> {
    let fail = |e: &dyn std::fmt::Display| format!("{} as {format}: {e}", ds.name);
    if format != "gpkg" {
        return write_bytes(ds, format).map(|(bytes, _)| bytes).map_err(|e| fail(&e));
    }
    let path = scratch.join("seed.gpkg");
    let _ = std::fs::remove_file(&path);
    write_path(ds, &path).map_err(|e| fail(&e))?;
    std::fs::read(&path).map_err(|e| fail(&e))
}

/// The folder `corpus/<target>`, made if missing.
fn corpus(target: &str) -> Result<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus").join(target);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

/// The sample project's layers, each cut to its first few features.
fn layers() -> Result<(Project, Vec<Dataset>)> {
    let mut s = Session::new(Project::new("seeds"));
    s.run("project.sample", &serde_json::Value::Null)?;
    let layers = s
        .map()
        .layers
        .iter()
        .map(|l| {
            let mut ds = l.dataset.clone();
            ds.features.truncate(FEATURES);
            ds
        })
        .collect();
    Ok((s.project.clone(), layers))
}

fn main() -> Result<()> {
    let (project, layers) = layers()?;

    let (read, sniff) = (corpus("read_layers")?, corpus("sniff")?);
    // GeoPackages are written here and read back: in the crate's own build folder, not a shared
    // temporary one, one folder per run, cleared of anything an earlier run with its id left.
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("seeds-scratch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(scratch.parent().unwrap_or(&scratch)).map_err(|e| format!("{}: {e}", scratch.display()))?;
    std::fs::create_dir(&scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    let (mut nr, mut ns) = (0, 0);
    for ds in &layers {
        for (format, ext) in WRITTEN {
            // Every format is written for every layer: a seed that can't be written is a mistake
            // here, not something to skip quietly.
            let bytes = written(ds, format, &scratch)?;
            let pick = READ_EXTENSIONS.iter().position(|e| e == ext).ok_or_else(|| format!("{ext} isn't read"))?;
            let pick = u8::try_from(pick).map_err(|e| e.to_string())?;
            seed(&read, &mut nr, &[&[pick][..], &bytes].concat())?;
            seed(&sniff, &mut ns, &bytes)?;
        }
    }

    let (dir, mut n) = (corpus("wkb")?, 0);
    for ds in &layers {
        for g in ds.features.iter().filter_map(|f| f.geometry.as_ref()).take(4) {
            seed(&dir, &mut n, &wkb::encode(g))?;
        }
    }

    let (dir, mut n) = (corpus("wkt")?, 0);
    for code in EPSG {
        let text = Crs::from_epsg(*code).and_then(wkt::to_wkt).ok_or_else(|| format!("EPSG:{code} has no WKT"))?;
        seed(&dir, &mut n, text.as_bytes())?;
    }

    let (dir, mut n) = (corpus("geotiff")?, 0);
    let transform = GeoTransform { origin_x: -90.05, origin_y: 35.2, pixel_width: 0.01, pixel_height: -0.01 };
    for (bands, color) in [(1, ColorHint::Gray), (3, ColorHint::Rgb { alpha: false }), (4, ColorHint::Rgb { alpha: true })] {
        let values = (0..bands).map(|b| (0..64).map(|i| (i * (b + 1)) as f32).collect()).collect();
        let raster = Raster::new("seed", 8, 8, values, transform, Crs::LonLat, None, color, "Float32").map_err(|e| e.to_string())?;
        let tiff = geotiff::write(&raster, 0).map_err(|e| e.to_string())?;
        seed(&dir, &mut n, &tiff)?;
        // A sniffed GeoTIFF goes to the raster reader: seed that branch of `sniff` too.
        seed(&sniff, &mut ns, &tiff)?;
    }

    let (dir, mut n) = (corpus("mvt")?, 0);
    let feature =
        |id, kind, geometry| mvt::Feature { id: Some(id), kind, geometry, props: vec![("name".into(), mvt::Value::String("seed".into()))] };
    let tile = mvt::Tile {
        layers: vec![mvt::Layer {
            name: "seed".into(),
            extent: 4096,
            features: vec![
                feature(1, mvt::GeomKind::Point, vec![vec![[2048.0, 2048.0]]]),
                feature(2, mvt::GeomKind::LineString, vec![vec![[0.0, 0.0], [4096.0, 4096.0]]]),
                feature(3, mvt::GeomKind::Polygon, vec![vec![[0.0, 0.0], [4096.0, 0.0], [4096.0, 4096.0], [0.0, 0.0]]]),
            ],
        }],
    };
    seed(&dir, &mut n, &mvt::encode(&tile))?;

    let (dir, mut n) = (corpus("project")?, 0);
    let mut small = project;
    for layer in &mut small.map_mut().layers {
        layer.dataset.features.truncate(FEATURES);
    }
    seed(&dir, &mut n, serde_json::to_string(&small).map_err(|e| e.to_string())?.as_bytes())?;
    seed(&dir, &mut n, serde_json::to_string(&Project::new("empty")).map_err(|e| e.to_string())?.as_bytes())?;

    let _ = std::fs::remove_dir_all(&scratch);
    println!("seeds written: read_layers {nr}, sniff {ns}");
    Ok(())
}
