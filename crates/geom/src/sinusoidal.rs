//! The sinusoidal projection (PROJ's `sinu`, GeoTIFF's coordinate transformation 24): the grid
//! of every MODIS land product, and ESRI:54008 (World Sinusoidal). proj4rs lacks it, so it is
//! computed here, for a raster's own coordinate system and for the map's alike.
//!
//! On a sphere x = R·(λ−λ₀)·cos φ and y = R·φ; on an ellipsoid x = a·(λ−λ₀)·cos φ / √(1−e²·sin²φ)
//! and y is the meridian distance, with PROJ's series for it and Newton's method for its inverse
//! (`pj_mlfn`, `pj_inv_mlfn`; PROJ is MIT, see `INCLUDED` in `xtask/src/notices.rs`). Both
//! directions are exact up to that series.

use crate::Coord;

/// A sinusoidal projection: the earth (semi-major axis and eccentricity squared, 0 for a sphere),
/// the central meridian, and the false easting and northing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sinusoidal {
    /// Semi-major axis, or the sphere's radius (metres).
    pub a: f64,
    /// Eccentricity squared: 0 for a sphere.
    pub es: f64,
    /// Central meridian (degrees).
    pub lon0: f64,
    /// False easting and northing (metres).
    pub x0: f64,
    pub y0: f64,
    /// The meridian distance series' coefficients.
    en: [f64; 5],
}

/// Axes (semi-major, inverse flattening) of the ellipsoids a definition names.
const ELLIPSOIDS: [(&str, f64, f64); 3] =
    [("WGS84", 6_378_137.0, 298.257_223_563), ("GRS80", 6_378_137.0, 298.257_222_101), ("clrk66", 6_378_206.4, 294.978_698_2)];

impl Sinusoidal {
    /// A projection on an earth of semi-major axis `a` (metres) and eccentricity squared `es`
    /// (0 for a sphere). `None` for an impossible earth or a parameter that isn't finite.
    pub fn new(a: f64, es: f64, lon0: f64, x0: f64, y0: f64) -> Option<Sinusoidal> {
        let ok = [a, es, lon0, x0, y0].iter().all(|v| v.is_finite()) && a > 0.0 && (0.0..1.0).contains(&es);
        ok.then(|| Sinusoidal { a, es, lon0, x0, y0, en: mlfn_coefficients(es) })
    }

    /// The projection a PROJ definition describes, when it is `+proj=sinu`: `None` for any other
    /// projection, an error for a sinusoidal one this can't compute exactly (a datum shift, a unit
    /// other than the metre, a parameter that isn't a number, or one it doesn't read: an axis
    /// order, grids, another sphere's radius).
    ///
    /// The earth is `+R`, else `+a` with `+b`, `+rf` or `+f`, else `+ellps` or `+datum`
    /// (WGS 84, GRS 1980, Clarke 1866), else GRS 1980 as in PROJ.
    pub fn from_proj4(def: &str) -> Option<Result<Sinusoidal, String>> {
        let params: Vec<(&str, &str)> =
            def.split_whitespace().take(64).map(|t| t.trim_start_matches('+')).map(|t| t.split_once('=').unwrap_or((t, ""))).collect();
        let get = |k: &str| params.iter().find(|(n, _)| *n == k).map(|(_, v)| *v);
        if get("proj") != Some("sinu") {
            return None;
        }
        Some((|| {
            let num = |k: &str| -> Result<Option<f64>, String> {
                match get(k) {
                    None => Ok(None),
                    Some(v) => v.parse::<f64>().ok().filter(|v| v.is_finite()).map(Some).ok_or_else(|| format!("+{k}={v} isn't a number")),
                }
            };
            // Only the parameters it reads (and `lat_0`, which PROJ's sinusoidal ignores too, and a
            // scale of 1): any other would be dropped, and the result wrong.
            const READ: [&str; 19] = [
                "proj", "R", "a", "b", "rf", "f", "ellps", "datum", "lon_0", "x_0", "y_0", "lat_0", "k", "k_0", "units", "to_meter",
                "towgs84", "pm", "no_defs",
            ];
            if let Some((k, _)) = params.iter().find(|(k, _)| !READ.contains(k) && *k != "type" && *k != "wktext") {
                return Err(format!("+{k} isn't supported in a sinusoidal projection"));
            }
            if ["k", "k_0"].iter().any(|k| get(k).is_some_and(|v| v.parse::<f64>().ok() != Some(1.0))) {
                return Err("a sinusoidal projection with a scale other than 1 isn't supported".into());
            }
            // Only what the formulas say: no datum shift (a zero one is none), metres.
            if get("towgs84").is_some_and(|v| !v.split(',').all(|c| c.trim().parse::<f64>().is_ok_and(|c| c == 0.0))) {
                return Err("a sinusoidal projection with a datum shift isn't supported".into());
            }
            if get("units").is_some_and(|u| u != "m") || get("to_meter").is_some_and(|m| m.parse::<f64>().ok() != Some(1.0)) {
                return Err("a sinusoidal projection in units other than metres isn't supported".into());
            }
            if get("pm").is_some_and(|p| p != "0" && p != "greenwich") {
                return Err("a sinusoidal projection with another prime meridian isn't supported".into());
            }
            let named = |n: &str| ELLIPSOIDS.iter().find(|(e, _, _)| *e == n).map(|(_, a, rf)| (*a, *rf));
            let (a, es) = if let Some(r) = num("R")? {
                (r, 0.0)
            } else if let Some(a) = num("a")? {
                let f = match (num("b")?, num("rf")?, num("f")?) {
                    (Some(b), _, _) => 1.0 - b / a,
                    (None, Some(rf), _) if rf > 0.0 => 1.0 / rf,
                    (None, _, Some(f)) => f,
                    _ => 0.0,
                };
                (a, f * (2.0 - f))
            } else {
                let name = match (get("ellps"), get("datum")) {
                    (Some(e), _) => e,
                    (None, Some("WGS84")) => "WGS84",
                    (None, Some("NAD83")) => "GRS80",
                    (None, Some("NAD27")) => "clrk66",
                    (None, Some(d)) => return Err(format!("unknown datum {d}")),
                    (None, None) => "GRS80",
                };
                let (a, rf) = named(name).ok_or_else(|| format!("unknown ellipsoid {name}"))?;
                let f = 1.0 / rf;
                (a, f * (2.0 - f))
            };
            let p = |k: &str| num(k).map(|v| v.unwrap_or(0.0));
            Sinusoidal::new(a, es, p("lon_0")?, p("x_0")?, p("y_0")?).ok_or_else(|| format!("{def}: not a possible sinusoidal projection"))
        })())
    }

    /// Longitude/latitude (degrees) to projected metres. Longitudes are taken from the central
    /// meridian the short way round, ±180° on their own side.
    pub fn forward(&self, c: Coord) -> Option<Coord> {
        if !c.is_finite() || c.y.abs() > 90.0 {
            return None;
        }
        let d = c.x - self.lon0;
        let d = if d.abs() <= 180.0 + 1e-9 { d.clamp(-180.0, 180.0) } else { (d + 540.0).rem_euclid(360.0) - 180.0 };
        let (lam, phi) = (d.to_radians(), c.y.to_radians());
        let (s, co) = phi.sin_cos();
        let x = lam * co / (1.0 - self.es * s * s).sqrt();
        let y = mlfn(phi, s, co, &self.en);
        let out = Coord::new(self.a * x + self.x0, self.a * y + self.y0);
        out.is_finite().then_some(out)
    }

    /// Projected metres to longitude/latitude (degrees); `None` outside the map's outline.
    pub fn inverse(&self, m: Coord) -> Option<Coord> {
        if !m.is_finite() {
            return None;
        }
        let (x, y) = ((m.x - self.x0) / self.a, (m.y - self.y0) / self.a);
        let phi = inv_mlfn(y, self.es, &self.en)?;
        let half = std::f64::consts::FRAC_PI_2;
        if phi.abs() > half + 1e-12 {
            return None;
        }
        let (s, co) = phi.sin_cos();
        let lam = if co.abs() < 1e-12 {
            // At a pole only the central meridian's x is on the map.
            if x.abs() > 1e-9 {
                return None;
            }
            0.0
        } else {
            x * (1.0 - self.es * s * s).sqrt() / co
        };
        // Past ±180° from the central meridian is off the outline.
        if lam.abs() > std::f64::consts::PI + 1e-9 {
            return None;
        }
        let lon = lam.to_degrees() + self.lon0;
        let lon = if (-180.0..=180.0).contains(&lon) { lon } else { (lon + 540.0).rem_euclid(360.0) - 180.0 };
        Some(Coord::new(lon, phi.clamp(-half, half).to_degrees()))
    }
}

/// The meridian distance series' coefficients for eccentricity squared `es` (PROJ's `pj_enfn`).
fn mlfn_coefficients(es: f64) -> [f64; 5] {
    const C00: f64 = 1.0;
    const C02: f64 = 0.25;
    const C04: f64 = 0.046875;
    const C06: f64 = 0.01953125;
    const C08: f64 = 0.01068115234375;
    const C22: f64 = 0.75;
    const C44: f64 = 0.46875;
    const C46: f64 = 0.013_020_833_333_333_333;
    const C48: f64 = 0.007_120_768_229_166_667;
    const C66: f64 = 0.364_583_333_333_333_3;
    const C68: f64 = 0.005_696_614_583_333_333;
    const C88: f64 = 0.3076171875;
    let mut t = es * es;
    let e0 = C00 - es * (C02 + es * (C04 + es * (C06 + es * C08)));
    let e1 = es * (C22 - es * (C04 + es * (C06 + es * C08)));
    let e2 = t * (C44 - es * (C46 + es * C48));
    t *= es;
    let e3 = t * (C66 - es * C68);
    let e4 = t * es * C88;
    [e0, e1, e2, e3, e4]
}

/// The meridian distance from the equator to latitude `phi` (radians), in semi-major axes.
fn mlfn(phi: f64, s: f64, c: f64, en: &[f64; 5]) -> f64 {
    let c = c * s;
    let s = s * s;
    en[0] * phi - c * (en[1] + s * (en[2] + s * (en[3] + s * en[4])))
}

/// The latitude (radians) whose meridian distance is `arg` semi-major axes; `None` when Newton's
/// method doesn't settle (a distance past the pole).
fn inv_mlfn(arg: f64, es: f64, en: &[f64; 5]) -> Option<f64> {
    let k = 1.0 / (1.0 - es);
    let mut phi = arg;
    for _ in 0..20 {
        let s = phi.sin();
        let t = 1.0 - es * s * s;
        let step = (mlfn(phi, s, phi.cos(), en) - arg) * (t * t.sqrt()) * k;
        phi -= step;
        if step.abs() < 1e-11 {
            return phi.is_finite().then_some(phi);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against pyproj 3.7.1 (PROJ 9.5.1).
    #[test]
    fn against_proj() {
        let close = |a: Coord, b: (f64, f64), tol: f64| (a.x - b.0).abs() < tol && (a.y - b.1).abs() < tol;
        // MODIS's sphere: Sorriso (Mato Grosso), and tile h12v10's corners, from metres.
        let modis =
            Sinusoidal::from_proj4("+proj=sinu +lon_0=0 +x_0=0 +y_0=0 +a=6371007.181 +b=6371007.181 +units=m +no_defs").unwrap().unwrap();
        assert_eq!(modis.es, 0.0);
        let m = modis.forward(Coord::new(-55.72, -12.55)).unwrap();
        assert!(close(m, (-6_047_750.816, -1_395_497.902), 0.01), "{m:?}");
        let ul = modis.inverse(Coord::new(-6_671_703.118, -1_111_950.519_667)).unwrap();
        assert!(close(ul, (-60.925_597, -10.0), 1e-6), "{ul:?}");
        let lr = modis.inverse(Coord::new(-5_559_752.598_333, -2_223_901.039_334)).unwrap();
        assert!(close(lr, (-53.208_889, -20.0), 1e-6), "{lr:?}");
        // ESRI:54008, World Sinusoidal on WGS 84: the meridian distance series.
        let world = Sinusoidal::from_proj4("+proj=sinu +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs").unwrap().unwrap();
        let w = world.forward(Coord::new(-55.72, -12.55)).unwrap();
        assert!(close(w, (-6_055_475.958, -1_387_927.939), 0.01), "{w:?}");
        let back = world.inverse(w).unwrap();
        assert!(close(back, (-55.72, -12.55), 1e-9), "{back:?}");
    }

    #[test]
    fn round_trips_outline_and_parameters() {
        for def in [
            "+proj=sinu +R=6371007.181",
            "+proj=sinu +ellps=GRS80 +lon_0=150 +x_0=500000 +y_0=-1000",
            "+proj=sinu +a=6378137 +rf=298.257223563",
        ] {
            let p = Sinusoidal::from_proj4(def).unwrap().unwrap();
            for (lon, lat) in [(0.0, 0.0), (10.0, 50.0), (-120.0, -35.0), (179.0, 89.0), (-60.0, 10.0), (149.0, -89.9)] {
                let m = p.forward(Coord::new(lon, lat)).unwrap();
                let back = p.inverse(m).unwrap_or_else(|| panic!("{def} {lon},{lat}"));
                let dl = ((back.x - lon + 540.0).rem_euclid(360.0) - 180.0).abs();
                assert!(dl < 1e-7 && (back.y - lat).abs() < 1e-9, "{def} {lon},{lat}: {back:?}");
            }
            // Beyond the outline: past ±180° from the central meridian, or past the pole.
            let edge = p.forward(Coord::new(p.lon0 + 180.0, 0.0)).unwrap();
            assert!(p.inverse(Coord::new(p.x0 + 1.01 * (edge.x - p.x0).abs(), p.y0)).is_none(), "{def}");
            assert!(p.inverse(Coord::new(p.x0 + 1_000_000.0, p.y0 + 10_100_000.0)).is_none(), "{def}");
            assert!(p.inverse(Coord::new(f64::NAN, 0.0)).is_none());
            assert!(p.forward(Coord::new(0.0, 91.0)).is_none());
        }
        // Not sinusoidal; sinusoidal it can't compute.
        assert!(Sinusoidal::from_proj4("+proj=robin +lon_0=0").is_none());
        for bad in [
            "+proj=sinu +R=x",
            "+proj=sinu +ellps=bessel",
            "+proj=sinu +datum=WGS84 +towgs84=1,2,3",
            "+proj=sinu +units=ft",
            "+proj=sinu +R=-1",
            "+proj=sinu +a=6378137 +b=8000000",
            "+proj=sinu +pm=paris",
            "+proj=sinu +axis=wsu +R=6371007.181",
            "+proj=sinu +datum=WGS84 +nadgrids=@null",
            "+proj=sinu +ellps=WGS84 +R_A",
            "+proj=sinu +R=6371007.181 +k=0.9996",
        ] {
            assert!(Sinusoidal::from_proj4(bad).unwrap().is_err(), "{bad}");
        }
        assert!(Sinusoidal::from_proj4("+proj=sinu +datum=WGS84 +towgs84=0,0,0").unwrap().is_ok());
    }
}
