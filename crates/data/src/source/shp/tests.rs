use super::*;
use crate::Dataset;
use crate::source::open_path;

fn dir() -> PathBuf {
    static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "rustgis-shp-index-test-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&path).unwrap();
    path
}
fn write_shapefile(ds: &Dataset, path: &Path) {
    crate::io::write_path(ds, path).unwrap();
}
fn points_shapefile(path: &Path) {
    let mut ds = Dataset::new("grid", GeometryType::Point, vec![]);
    for x in 0..30 {
        for y in 0..20 {
            ds.push(Some(Geometry::Point(Coord::new(x as f64, y as f64))), vec![]);
        }
    }
    write_shapefile(&ds, path);
}
#[test]
fn shapefile_disk_index_reopens_and_invalidates_without_changing_source() {
    let fixture = dir();
    cache::with_index_dir(fixture.join("cache"), || {
        let p = fixture.join("indexed-grid.shp");
        points_shapefile(&p);
        let original = std::fs::read(&p).unwrap();
        let req = Request { extent: Some(Extent::new(10.0, 10.0, 40.0, 30.0)), limit: 1000 };
        let src = open_path(&p, None).unwrap();
        let expected = src.read(&req).unwrap();
        let reopened = open_path(&p, None).unwrap();
        assert!(reopened.info().indexed);
        assert_eq!(reopened.read(&req).unwrap().features, expected.features);
        assert_eq!(std::fs::read(&p).unwrap(), original);
        drop((src, reopened));
        // Replacing the same path must never reuse stale offsets, boxes or record counts.
        let mut replacement = Dataset::new("new", GeometryType::Point, vec![]);
        replacement.push(Some(Geometry::Point(Coord::new(-75.0, -30.0))), vec![]);
        write_shapefile(&replacement, &p);
        let changed = open_path(&p, None).unwrap();
        assert_eq!(changed.info().count, Some(1));
        assert!(changed.read(&req).unwrap().features.is_empty());
        assert_eq!(changed.read(&Request::all()).unwrap().features.len(), 1);
    });
    std::fs::remove_dir_all(fixture).unwrap();
}

#[test]
fn shapefile_disk_index_keeps_file_order_and_query_boundaries() {
    let fixture = dir();
    cache::with_index_dir(fixture.join("cache"), || {
        let p = fixture.join("indexed-order.shp");
        let mut ds = Dataset::new("ordered", GeometryType::Point, vec![]);
        // Deliberately reverse spatial order. The index must not change painter/attribute order.
        for x in (0..1000).rev() {
            ds.push(Some(Geometry::Point(Coord::new(x as f64 / 100.0, 0.0))), vec![]);
        }
        write_shapefile(&ds, &p);
        // A stale file-header envelope cannot prove that a view contains the whole source.
        let mut bytes = std::fs::read(&p).unwrap();
        for (at, value) in [(36, 1.5f64), (44, 0.0), (52, 1.5), (60, 0.0)] {
            bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
        std::fs::write(&p, bytes).unwrap();
        let src = open_path(&p, None).unwrap();
        let batch = src.read(&Request { extent: Some(Extent::new(1.0, -1.0, 2.0, 1.0)), limit: 1000 }).unwrap();
        assert_eq!(batch.matched, 101);
        assert_eq!(batch.features.len(), 101);
        assert!(batch.complete);
        assert!(batch.features.windows(2).all(|p| p[0].id < p[1].id));
        assert_eq!(batch.features.first().unwrap().geometry, Some(Geometry::Point(Coord::new(2.0, 0.0))));
        assert_eq!(batch.features.last().unwrap().geometry, Some(Geometry::Point(Coord::new(1.0, 0.0))));
        let sample = src.read(&Request { extent: None, limit: 10 }).unwrap();
        assert_eq!(sample.matched, 1000);
        assert_eq!(sample.features.len(), 10);
        assert!(!sample.complete);
        assert_eq!(sample.features.iter().map(|f| f.id).collect::<Vec<_>>(), (0..10).map(|i| i * 100 + 1).collect::<Vec<_>>());
    });
    std::fs::remove_dir_all(fixture).unwrap();
}

#[test]
fn shapefile_overview_index_shortcut_does_not_include_null_shapes() {
    let fixture = dir();
    cache::with_index_dir(fixture.join("cache"), || {
        let p = fixture.join("indexed-null.shp");
        let mut ds = Dataset::new("point", GeometryType::Point, vec![]);
        ds.push(Some(Geometry::Point(Coord::new(1.0, 1.0))), vec![]);
        write_shapefile(&ds, &p);
        let mut bytes = std::fs::read(&p).unwrap();
        bytes.extend_from_slice(&2i32.to_be_bytes());
        bytes.extend_from_slice(&2i32.to_be_bytes());
        bytes.extend_from_slice(&0i32.to_le_bytes());
        let words = bytes.len() as i32 / 2;
        bytes[24..28].copy_from_slice(&words.to_be_bytes());
        std::fs::write(&p, bytes).unwrap();
        let src = open_path(&p, None).unwrap();
        assert_eq!(src.read(&Request::all()).unwrap().features.len(), 2);
        let view = src.read(&Request { extent: Some(Extent::new(-10.0, -10.0, 10.0, 10.0)), limit: 100 }).unwrap();
        assert_eq!(view.matched, 1);
        assert_eq!(view.features.len(), 1);
        assert!(view.features.iter().all(|f| f.geometry.is_some()));
    });
    std::fs::remove_dir_all(fixture).unwrap();
}

#[test]
fn disk_index_is_reused_and_corrupt_cache_is_rebuilt() {
    let folder = dir();
    let source = folder.join("points.shp");
    points_shapefile(&source);
    let cache_root = folder.join("cache");
    cache::with_index_dir(cache_root.clone(), || {
        drop(ShpSource::open_path(&source, "points").unwrap());
        let path = std::fs::read_dir(&cache_root)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "sqlite"))
            .unwrap();
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE reuse_probe(value INTEGER)").unwrap();
        drop(db);
        drop(ShpSource::open_path(&source, "points").unwrap());
        let db = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(db.query_row("SELECT count(*) FROM sqlite_master WHERE name='reuse_probe'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        drop(db);
        std::fs::write(&path, b"interrupted or corrupted cache").unwrap();
        let reopened = ShpSource::open_path(&source, "points").unwrap();
        assert_eq!(reopened.read(&Request::all()).unwrap().features.len(), 600);
        drop(reopened);
        cache::maintain(&cache_root, 0).unwrap();
    });
    std::fs::remove_dir_all(folder).unwrap();
}

/// Opening costs the same whatever the cache holds: building an index sweeps the cache folder
/// once, reusing one sweeps it not at all, and neither release does (#1230).
#[test]
fn opening_sweeps_the_cache_only_to_build_an_index() {
    let folder = dir();
    let source = folder.join("points.shp");
    points_shapefile(&source);
    let cache_root = folder.join("cache");
    cache::with_index_dir(cache_root.clone(), || {
        let before = cache::sweeps();
        let built = ShpSource::open_path(&source, "points").unwrap();
        assert!(built.info().indexed);
        assert_eq!(cache::sweeps(), before + 1, "building an index sweeps once");
        drop(built);
        let reused = ShpSource::open_path(&source, "points").unwrap();
        assert!(reused.info().indexed);
        assert_eq!(reused.read(&Request::all()).unwrap().features.len(), 600);
        drop(reused);
        assert_eq!(cache::sweeps(), before + 1, "reusing an index and releasing either sweeps nothing");
        cache::maintain(&cache_root, 0).unwrap();
    });
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn disk_queries_match_legacy_scan_sampling_and_edges() {
    let folder = dir();
    let source = folder.join("points.shp");
    let mut ds = Dataset::new("points", GeometryType::Point, vec![]);
    for i in 0..401 {
        let x = ((i * 71) % 401) as f64 / 13.0 - 15.0;
        let y = ((i * 37) % 401) as f64 / 17.0 - 12.0;
        ds.push(Some(Geometry::Point(Coord::new(x, y))), vec![]);
    }
    write_shapefile(&ds, &source);
    cache::with_index_dir(folder.join("cache"), || {
        let src = ShpSource::open_path(&source, "points").unwrap();
        for extent in [
            None,
            Some(Extent::new(-180.0, -90.0, 180.0, 90.0)),
            Some(Extent::new(-4.5, -3.0, 7.5, 4.0)),
            Some(Extent::new(80.0, 50.0, 90.0, 60.0)),
        ] {
            let ids: Vec<u64> = ds
                .features
                .iter()
                .enumerate()
                .filter_map(|(i, f)| {
                    let Some(Geometry::Point(p)) = &f.geometry else { return None };
                    let mut raw = Vec::new();
                    raw.extend_from_slice(&1i32.to_le_bytes());
                    raw.extend_from_slice(&p.x.to_le_bytes());
                    raw.extend_from_slice(&p.y.to_le_bytes());
                    let b = record_box(&raw);
                    extent
                        .is_none_or(|e| {
                            b[0] <= e.max_x as f32 && b[2] >= e.min_x as f32 && b[1] <= e.max_y as f32 && b[3] >= e.min_y as f32
                        })
                        .then_some(i as u64 + 1)
                })
                .collect();
            for limit in [0, 1, 7, 29, 1000] {
                let step = crate::source::stride(ids.len() as u64, limit).max(1) as usize;
                let expected: Vec<_> = ids.iter().copied().step_by(step).take(limit).collect();
                let batch = src.read(&Request { extent, limit }).unwrap();
                assert_eq!(batch.matched, ids.len() as u64);
                assert_eq!(batch.complete, step == 1);
                assert_eq!(batch.features.iter().map(|f| f.id).collect::<Vec<_>>(), expected);
            }
        }
    });
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
#[ignore = "manual cold/warm index benchmark; creates 100,000 synthetic records"]
fn shapefile_disk_index_benchmark() {
    let folder = dir();
    let path = folder.join("benchmark.shp");
    let mut ds = Dataset::new("benchmark", GeometryType::Point, vec![]);
    for i in 0..100_000 {
        ds.push(Some(Geometry::Point(Coord::new((i % 1000) as f64 / 10.0 - 50.0, (i / 1000) as f64 / 10.0 - 5.0))), vec![]);
    }
    write_shapefile(&ds, &path);
    cache::with_index_dir(folder.join("cache"), || {
        let clock = std::time::Instant::now();
        let first = ShpSource::open_path(&path, "benchmark").unwrap();
        let cold = clock.elapsed();
        drop(first);
        let clock = std::time::Instant::now();
        let source = ShpSource::open_path(&path, "benchmark").unwrap();
        let warm = clock.elapsed();
        let request = Request { extent: Some(Extent::new(-1.0, -1.0, 1.0, 1.0)), limit: 1000 };
        let clock = std::time::Instant::now();
        let indexed = source.read(&request).unwrap();
        let query = clock.elapsed();
        let clock = std::time::Instant::now();
        let mut matched = 0u64;
        let bytes = std::fs::read(&path).unwrap();
        for raw in bytes.get(100..).unwrap().as_chunks::<28>().0 {
            let [x0, y0, x1, y1] = record_box(raw.get(8..).unwrap());
            if x0 <= 1.0 && x1 >= -1.0 && y0 <= 1.0 && y1 >= -1.0 {
                matched += 1;
            }
        }
        let scan = clock.elapsed();
        assert_eq!(indexed.matched, matched);
        eprintln!(
            "100000 points: cold={cold:?}, warm={warm:?}, indexed-query={query:?}, record-header scan including file read={scan:?}, matches={matched}"
        );
    });
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn shapefile_disk_index_complete_chunks_preserve_full_reads_and_file_order() {
    let fixture = dir();
    cache::with_index_dir(fixture.join("cache"), || {
        let path = fixture.join("complete-indexed.shp");
        points_shapefile(&path);
        let source = open_path(&path, None).unwrap();
        assert!(source.info().indexed);
        assert_eq!(source.read(&Request { extent: None, limit: 1 }).unwrap().features.len(), 1);
        for extent in [None, Some(Extent::new(10.0, 5.0, 15.0, 10.0)), Some(Extent::EMPTY)] {
            let expected = source.read(&Request { extent, ..Request::all() }).unwrap();
            for budget in [1, 256, 8192] {
                let mut rows = Vec::new();
                let summary = source
                    .visit_chunks(extent, budget, &mut |chunk| {
                        assert!(chunk.len() == 1 || crate::source::chunk_memory::features_bytes(&chunk) <= budget);
                        rows.extend(chunk);
                        Ok(())
                    })
                    .unwrap();
                assert!(summary.complete);
                assert!(summary.features.is_empty());
                assert_eq!(summary.matched, expected.matched);
                assert_eq!(summary.skipped, expected.skipped);
                assert_eq!(rows, expected.features);
            }
        }
        for extent in [None, Some(Extent::EMPTY), Some(Extent::new(-180.0, -90.0, -179.0, -89.0))] {
            assert!(source.visit_chunks(extent, 0, &mut |_| panic!("zero budget must not emit")).is_err());
        }
        let mut reentrant = false;
        source
            .visit_chunks(None, 256, &mut |_| {
                if !reentrant {
                    assert_eq!(source.read(&Request { extent: None, limit: 1 })?.features.len(), 1);
                    reentrant = true;
                }
                Ok(())
            })
            .unwrap();
        assert!(reentrant);
        let mut calls = 0;
        assert!(
            source
                .visit_chunks(None, 1, &mut |_| {
                    calls += 1;
                    Err(bad("cancelled indexed traversal"))
                })
                .is_err()
        );
        assert_eq!(calls, 1);
        assert!(source.read(&Request::all()).unwrap().complete, "cancellation must release the index lock");
    });
    std::fs::remove_dir_all(fixture).unwrap();
}

#[test]
fn shapefile_record_past_the_cap_is_skipped_not_allocated() {
    // A record whose header claims more than MAX_RECORD bytes, inside a file grown (sparse where
    // the file system allows) to hold it: skipped and counted, never given a buffer that size.
    let fixture = dir();
    cache::with_index_dir(fixture.join("cache"), || {
        let p = fixture.join("huge-record.shp");
        let mut ds = Dataset::new("two", GeometryType::Point, vec![]);
        ds.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![]);
        ds.push(Some(Geometry::Point(Coord::new(3.0, 4.0))), vec![]);
        write_shapefile(&ds, &p);
        // The second record's header is at 100 + 28; its content length (16-bit words) at +4.
        let bytes = MAX_RECORD + 2;
        let mut shp = std::fs::read(&p).unwrap();
        shp.splice(132..136, i32::try_from(bytes / 2).unwrap().to_be_bytes());
        std::fs::write(&p, &shp).unwrap();
        std::fs::OpenOptions::new().write(true).open(&p).unwrap().set_len(136 + bytes).unwrap();
        let src = open_path(&p, None).unwrap();
        let all = src.read(&Request::all()).unwrap();
        assert_eq!((all.features.len(), all.skipped), (1, 1));
        let mut chunked = 0;
        let batch = src
            .visit_chunks(None, 1 << 20, &mut |f| {
                chunked += f.len();
                Ok(())
            })
            .unwrap();
        assert_eq!((chunked, batch.skipped), (1, 1));
    });
    std::fs::remove_dir_all(fixture).unwrap();
}
