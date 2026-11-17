//! Trends in a time series (opengeos/RustGIS#579): Sen's slope for the rate of change, and the
//! Mann–Kendall test for whether it is significant, in the variants used on autocorrelated
//! series such as NDVI and temperature.
//!
//! - **Hamed & Rao (1998):** the variance of S is corrected by the autocorrelation of the ranks of
//!   the series with Sen's trend taken out, using only the lags significant at the test's level.
//! - **Yue & Wang (2004):** the variance is corrected by an effective sample size, from the lag-1
//!   autocorrelation of the series with the trend taken out (`pymannkendall` with `lag=1`, R's
//!   `modifiedmk::mmky1lag`). Summing every lag instead makes white noise look correlated: the
//!   lags of a short sample's autocorrelation sum to about −½.
//! - **Seasonal (Hirsch et al. 1982):** the test is made within each season (the same month, or
//!   the same part of the year) and the seasons' statistics are summed; Sen's slope is the median
//!   of the slopes between values of the same season.
//! - **Original:** Mann (1945) and Kendall (1975), for independent values.
//!
//! The variants follow `pymannkendall`'s definitions, so results can be checked against it. One
//! difference: the corrected tests take the trend out at each value's own time, where
//! `pymannkendall` takes it out at the value's place (0, 1, 2…); the two agree on evenly spaced
//! series, and on uneven ones (gaps, a months filter) this keeps the real spacing. Each series is
//! the values at its times; missing values (not finite) are skipped. Times are in any unit (years,
//! say): the slope is per that unit.

/// The Mann–Kendall test to make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Test {
    /// Hamed & Rao's correction for autocorrelation (the default).
    HamedRao,
    /// Yue & Wang's correction for autocorrelation, from the lag-1 autocorrelation.
    YueWang,
    /// The seasonal test, with this many seasons in a unit of time (12: months of a year). Through
    /// [`trend`], a value's season is its time's place within its unit (times in years: 2000.5 is
    /// mid-2000), so times counted in steps (0, 1, 2…) put every value in one season; for real
    /// dates, [`trend_seasonal`] takes each value's season from its calendar.
    Seasonal(u32),
    /// The test for independent values.
    Original,
}

impl Test {
    /// The test named `name` (`hamedRao`, `yueWang`, `seasonal`, `original`), with `seasons` for
    /// the seasonal one.
    pub fn parse(name: &str, seasons: u32) -> Option<Test> {
        let n = name.trim().to_ascii_lowercase().replace(['-', '_', ' ', '&'], "");
        Some(match n.as_str() {
            "" | "hamedrao" | "modified" | "mmk" => Test::HamedRao,
            "yuewang" => Test::YueWang,
            "seasonal" => Test::Seasonal(seasons.clamp(2, 366)),
            "original" | "mannkendall" | "mk" => Test::Original,
            _ => return None,
        })
    }

    /// Its short name, for layer names.
    pub fn code(self) -> &'static str {
        match self {
            Test::HamedRao => "Hamed-Rao",
            Test::YueWang => "Yue-Wang",
            Test::Seasonal(_) => "seasonal",
            Test::Original => "Mann-Kendall",
        }
    }
}

/// A series' trend.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trend {
    /// The values used (finite ones; for the seasonal test, those in seasons of two or more,
    /// which are the ones S and the slope come from).
    pub n: usize,
    /// Sen's slope, per unit of time.
    pub slope: f64,
    /// Sen's intercept, as `pymannkendall` and Conover (1980) give it: the median value less the
    /// slope times the median time.
    pub intercept: f64,
    /// Mann–Kendall's S.
    pub s: f64,
    /// S's variance, corrected for autocorrelation by the modified tests.
    pub var_s: f64,
    /// The modified tests' correction, n/n* (1 for the others). Where it would be zero or below
    /// (strong negative autocorrelation on a short series), which would leave S without a
    /// variance, it is 1: the uncorrected test, where `pymannkendall` gives no p-value.
    pub correction: f64,
    /// The standard normal statistic.
    pub z: f64,
    /// The two-sided p-value.
    pub p: f64,
}

/// The fewest values a trend is worked out from.
pub const MIN_VALUES: usize = 4;
/// The most: Sen's slope keeps the slope of every two, so this bounds its memory (4,000 values,
/// about 64 MB, on the desktop; 2,000, 16 MB, in the browser's smaller memory). A data cube's
/// trend takes fewer still (2,000 dates).
pub const MAX_VALUES: usize = if cfg!(target_arch = "wasm32") { 2_000 } else { 4_000 };

/// The trend of the values `y` at times `t` (the same length, in any order: they're taken in time
/// order; missing values skipped). `alpha` is the test's level, between 0 and 1 (Hamed & Rao's
/// correction also uses it to choose its lags). `None` when fewer than [`MIN_VALUES`] or more than
/// [`MAX_VALUES`] are left, for an `alpha` outside 0 to 1, or, for the seasonal test, when no
/// season has two values.
pub fn trend(t: &[f64], y: &[f64], test: Test, alpha: f64) -> Option<Trend> {
    trend_with(t, y, test, alpha, &mut Vec::new())
}

/// [`trend`], keeping Sen's slopes in `slopes` (cleared first): a caller working out many series
/// in turn (a cube, cell by cell) keeps one buffer and spares the largest allocation (n²⁄2
/// slopes) each time; the series' own vectors, of n values, are still made per call.
pub fn trend_with(t: &[f64], y: &[f64], test: Test, alpha: f64, slopes: &mut Vec<f64>) -> Option<Trend> {
    let mut kept: Vec<(f64, f64)> = t.iter().zip(y).filter(|(t, y)| t.is_finite() && y.is_finite()).map(|(t, y)| (*t, *y)).collect();
    // The test reads the values in time order (S, the autocorrelation).
    kept.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (t, y): (Vec<f64>, Vec<f64>) = kept.into_iter().unzip();
    let n = y.len();
    if !((MIN_VALUES..=MAX_VALUES).contains(&n) && alpha > 0.0 && alpha < 1.0) {
        return None;
    }
    if let Test::Seasonal(m) = test {
        return seasonal(&t, &y, m, slopes);
    }
    let slope = sen_slope_in(&t, &y, slopes)?;
    let intercept = intercept(&t, &y, slope)?;
    let s = score(&y);
    let mut var_s = variance(&y);
    let mut correction = 1.0;
    let nf = n as f64;
    match test {
        Test::HamedRao => {
            // The ranks of the series with the trend taken out, and their autocorrelation.
            let detrended: Vec<f64> = t.iter().zip(&y).map(|(t, y)| y - slope * t).collect();
            let acf = autocorrelation(&ranks(&detrended));
            let bound = normal_quantile(1.0 - alpha / 2.0) / nf.sqrt();
            let sum: f64 = acf
                .iter()
                .enumerate()
                .skip(1)
                .filter(|(_, r)| r.abs() > bound)
                .map(|(k, r)| {
                    let k = k as f64;
                    (nf - k) * (nf - k - 1.0) * (nf - k - 2.0) * r
                })
                .sum();
            correction = 1.0 + 2.0 / (nf * (nf - 1.0) * (nf - 2.0)) * sum;
        }
        Test::YueWang => {
            let detrended: Vec<f64> = t.iter().zip(&y).map(|(t, y)| y - slope * t).collect();
            let r1 = autocorrelation(&detrended).get(1).copied().unwrap_or(0.0);
            correction = 1.0 + 2.0 * (1.0 - 1.0 / nf) * r1;
        }
        _ => {}
    }
    // A correction below zero (strong negative autocorrelation on a short series) would make
    // the variance meaningless: it's kept positive.
    if !(correction.is_finite() && correction > 0.0) {
        correction = 1.0;
    }
    var_s *= correction;
    let z = z_score(s, var_s);
    Some(Trend { n, slope, intercept, s, var_s, correction, z, p: p_value(z) })
}

/// Sen's slope: the median of the slopes between every two values at different times; `None`
/// for more than [`MAX_VALUES`], whose slopes would take too much memory.
pub fn sen_slope(t: &[f64], y: &[f64]) -> Option<f64> {
    sen_slope_in(t, y, &mut Vec::new())
}

/// [`sen_slope`] with its slopes kept in `slopes` (cleared first).
fn sen_slope_in(t: &[f64], y: &[f64], slopes: &mut Vec<f64>) -> Option<f64> {
    let n = y.len().min(t.len());
    if n > MAX_VALUES {
        return None;
    }
    slopes.clear();
    push_slopes(&t[..n], &y[..n], slopes);
    median(slopes)
}

/// The slopes between every two values at different times, added to `out`: the one rule for
/// Sen's slope and the seasonal test's. A slope too steep to hold (huge values, times a hair
/// apart) is left out. Its callers bound the values (at most [`MAX_VALUES`]), so `out` stays
/// within `MAX_VALUES`² ⁄ 2.
fn push_slopes(t: &[f64], y: &[f64], out: &mut Vec<f64>) {
    for (i, (ti, yi)) in t.iter().zip(y).enumerate() {
        for (tj, yj) in t.iter().zip(y).skip(i + 1) {
            let dt = tj - ti;
            let slope = (yj - yi) / dt;
            if dt != 0.0 && slope.is_finite() {
                out.push(slope);
            }
        }
    }
}

/// Sen's intercept: the median value less the slope times the median time.
fn intercept(t: &[f64], y: &[f64], slope: f64) -> Option<f64> {
    Some(median(&mut y.to_vec())? - slope * median(&mut t.to_vec())?)
}

/// The seasonal test: S and its variance summed over the seasons, and Sen's slope from pairs
/// within a season.
fn seasonal(t: &[f64], y: &[f64], seasons: u32, slopes: &mut Vec<f64>) -> Option<Trend> {
    let m = seasons.max(1) as usize;
    // The season of a time: its place within its unit (year), in `m` equal parts.
    // A hair above, so a time computed on a season's start (2000 + 3⁄12) isn't put in the one before.
    let season: Vec<usize> = t.iter().map(|t| (((t - t.floor()) * m as f64 + 1e-9).floor() as usize).min(m - 1)).collect();
    seasonal_by(t, y, &season, slopes)
}

/// The seasonal test with each value's season given (its month, or its part of the year as the
/// calendar has it): what a series of real dates should use, since a time's fraction of a year
/// drifts against the calendar from year to year. Missing values are skipped as by [`trend`];
/// `None` as it gives one, or when the three lists aren't the same length.
pub fn trend_seasonal(t: &[f64], y: &[f64], season: &[usize], alpha: f64) -> Option<Trend> {
    trend_seasonal_with(t, y, season, alpha, &mut Vec::new())
}

/// [`trend_seasonal`], keeping the slopes in `slopes` (cleared first), as [`trend_with`] does.
pub fn trend_seasonal_with(t: &[f64], y: &[f64], season: &[usize], alpha: f64, slopes: &mut Vec<f64>) -> Option<Trend> {
    if y.len() != t.len() || season.len() != t.len() {
        return None;
    }
    let mut kept: Vec<(f64, f64, usize)> =
        t.iter().zip(y).zip(season).filter(|((t, y), _)| t.is_finite() && y.is_finite()).map(|((t, y), s)| (*t, *y, *s)).collect();
    kept.sort_by(|a, b| a.0.total_cmp(&b.0));
    if !((MIN_VALUES..=MAX_VALUES).contains(&kept.len()) && alpha > 0.0 && alpha < 1.0) {
        return None;
    }
    let t: Vec<f64> = kept.iter().map(|k| k.0).collect();
    let y: Vec<f64> = kept.iter().map(|k| k.1).collect();
    let s: Vec<usize> = kept.iter().map(|k| k.2).collect();
    seasonal_by(&t, &y, &s, slopes)
}

fn seasonal_by(t: &[f64], y: &[f64], season: &[usize], slopes: &mut Vec<f64>) -> Option<Trend> {
    // A season index past any calendar's (366 days) is a caller's mistake: no trend, not a huge allocation.
    let m = season.iter().copied().max()?.checked_add(1).filter(|m| *m <= 4096)?;
    let mut by: Vec<(Vec<f64>, Vec<f64>)> = vec![(Vec::new(), Vec::new()); m];
    for ((t, y), k) in t.iter().zip(y).zip(season) {
        let Some(b) = by.get_mut(*k) else { continue };
        b.0.push(*t);
        b.1.push(*y);
    }
    let (mut s, mut var_s, mut n) = (0.0, 0.0, 0);
    slopes.clear();
    for (ts, ys) in by.iter().filter(|(_, ys)| ys.len() >= 2) {
        s += score(ys);
        var_s += variance(ys);
        n += ys.len();
        push_slopes(ts, ys, slopes);
    }
    if n < MIN_VALUES {
        return None;
    }
    let slope = median(slopes)?;
    // The intercept from every value, those of single-value seasons too (as pymannkendall's
    // seasonal test takes the medians of the whole series); `n`, S and the slope from the pairs.
    let intercept = intercept(t, y, slope)?;
    let z = z_score(s, var_s);
    Some(Trend { n, slope, intercept, s, var_s, correction: 1.0, z, p: p_value(z) })
}

/// Mann–Kendall's S: the signs of every later value minus every earlier one, summed.
fn score(y: &[f64]) -> f64 {
    let mut s = 0i64;
    for (i, yi) in y.iter().enumerate() {
        for yj in y.iter().skip(i + 1) {
            s += match yj.partial_cmp(yi) {
                Some(std::cmp::Ordering::Greater) => 1,
                Some(std::cmp::Ordering::Less) => -1,
                _ => 0,
            };
        }
    }
    s as f64
}

/// S's variance for independent values, corrected for ties.
fn variance(y: &[f64]) -> f64 {
    let n = y.len() as f64;
    let mut sorted = y.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mut ties = 0.0;
    let mut i = 0;
    while i < sorted.len() {
        let mut j = i + 1;
        while j < sorted.len() && sorted[j] == sorted[i] {
            j += 1;
        }
        let t = (j - i) as f64;
        if t > 1.0 {
            ties += t * (t - 1.0) * (2.0 * t + 5.0);
        }
        i = j;
    }
    (n * (n - 1.0) * (2.0 * n + 5.0) - ties) / 18.0
}

/// The normal statistic of S, with the continuity correction.
fn z_score(s: f64, var_s: f64) -> f64 {
    if var_s <= 0.0 || !var_s.is_finite() {
        return 0.0;
    }
    if s > 0.0 {
        (s - 1.0) / var_s.sqrt()
    } else if s < 0.0 {
        (s + 1.0) / var_s.sqrt()
    } else {
        0.0
    }
}

/// The two-sided p-value of a standard normal statistic.
pub fn p_value(z: f64) -> f64 {
    erfc(z.abs() / std::f64::consts::SQRT_2).clamp(0.0, 1.0)
}

/// The median, reordering `v`; `None` when it's empty.
fn median(v: &mut [f64]) -> Option<f64> {
    let n = v.len();
    if n == 0 {
        return None;
    }
    let mid = n / 2;
    let (_, m, _) = v.select_nth_unstable_by(mid, f64::total_cmp);
    let m = *m;
    if n % 2 == 1 {
        return Some(m);
    }
    // The other middle value is the largest of the lower half.
    let lower = v[..mid].iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Some((lower + m) / 2.0)
}

/// Ranks from 1, ties sharing their mean rank. Values within rounding of each other tie: a
/// series detrended by Sen's slope has two values that are equal (the pair the median slope came
/// from), which arithmetic in another order leaves an ulp apart.
fn ranks(y: &[f64]) -> Vec<f64> {
    let mut order: Vec<usize> = (0..y.len()).collect();
    order.sort_by(|a, b| y[*a].total_cmp(&y[*b]));
    let scale = y.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    let tol = scale * 1e-12;
    let mut out = vec![0.0; y.len()];
    let mut i = 0;
    while i < order.len() {
        let mut j = i + 1;
        while j < order.len() && y[order[j]] - y[order[i]] <= tol {
            j += 1;
        }
        let rank = (i + j + 1) as f64 / 2.0;
        for k in &order[i..j] {
            out[*k] = rank;
        }
        i = j;
    }
    out
}

/// The autocorrelation at every lag from 0 to n − 1 (the sum of products over the whole
/// series' sum of squares, as `statsmodels` computes it).
fn autocorrelation(x: &[f64]) -> Vec<f64> {
    let n = x.len();
    let mean = x.iter().sum::<f64>() / n.max(1) as f64;
    let d: Vec<f64> = x.iter().map(|v| v - mean).collect();
    let denom: f64 = d.iter().map(|v| v * v).sum();
    if denom == 0.0 {
        return vec![0.0; n];
    }
    (0..n).map(|k| d[..n - k].iter().zip(&d[k..]).map(|(a, b)| a * b).sum::<f64>() / denom).collect()
}

/// The complementary error function, to about 1e-15 (W. J. Cody's rational approximations, as
/// in most mathematical libraries).
pub fn erfc(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    if x < 0.0 {
        return 2.0 - erfc(-x);
    }
    if x <= 0.468_75 {
        return 1.0 - erf_small(x);
    }
    if x > 27.0 {
        return 0.0;
    }
    // erfc(x) = exp(−x²) · R(x), R from Cody's coefficients for 0.46875 < x < 4 and x ≥ 4.
    let r = if x < 4.0 {
        const P: [f64; 9] = [
            5.641_884_969_886_701e-1,
            8.883_149_794_388_375,
            6.611_919_063_714_163e1,
            2.986_351_381_974_001e2,
            8.819_522_212_417_69e2,
            1.712_047_612_634_070_7e3,
            2.051_078_377_826_071_6e3,
            1.230_339_354_797_997_2e3,
            2.153_115_354_744_038_3e-8,
        ];
        const Q: [f64; 8] = [
            1.574_492_611_070_983_5e1,
            1.176_939_508_913_125e2,
            5.371_811_018_620_099e2,
            1.621_389_574_566_690_3e3,
            3.290_799_235_733_459_7e3,
            4.362_619_090_143_247e3,
            3.439_367_674_143_721_6e3,
            1.230_339_354_803_749_4e3,
        ];
        let mut num = P[8] * x;
        let mut den = x;
        for i in 0..7 {
            num = (num + P[i]) * x;
            den = (den + Q[i]) * x;
        }
        (num + P[7]) / (den + Q[7])
    } else {
        const P: [f64; 6] = [
            3.053_266_349_612_323_4e-1,
            3.603_448_999_498_044_5e-1,
            1.257_817_261_112_292_6e-1,
            1.608_378_514_874_227_5e-2,
            6.587_491_615_298_378e-4,
            1.631_538_713_730_709_6e-2,
        ];
        const Q: [f64; 5] = [
            2.568_520_192_289_822,
            1.872_952_849_923_460_4,
            5.279_051_029_514_285e-1,
            6.051_834_131_244_132e-2,
            2.335_204_976_268_691_8e-3,
        ];
        let z = 1.0 / (x * x);
        let mut num = P[5] * z;
        let mut den = z;
        for i in 0..4 {
            num = (num + P[i]) * z;
            den = (den + Q[i]) * z;
        }
        let r = z * (num + P[4]) / (den + Q[4]);
        (1.0 / std::f64::consts::PI.sqrt() - r) / x
    };
    // exp(−x²) in two parts, so its rounding error doesn't grow with x.
    let xs = (x * 16.0).trunc() / 16.0;
    let del = (x - xs) * (x + xs);
    (-xs * xs).exp() * (-del).exp() * r
}

/// erf(x) for |x| ≤ 0.46875 (Cody).
fn erf_small(x: f64) -> f64 {
    const A: [f64; 5] =
        [3.161_123_743_870_565_6, 1.138_641_541_510_501_6e2, 3.774_852_376_853_02e2, 3.209_377_589_138_469_5e3, 1.857_777_061_846_031_5e-1];
    const B: [f64; 4] = [2.360_129_095_234_412_1e1, 2.440_246_379_344_441_7e2, 1.282_616_526_077_372_3e3, 2.844_236_833_439_171e3];
    let z = x * x;
    let mut num = A[4] * z;
    let mut den = z;
    for i in 0..3 {
        num = (num + A[i]) * z;
        den = (den + B[i]) * z;
    }
    x * (num + A[3]) / (den + B[3])
}

/// The standard normal quantile (Acklam's approximation, refined by one Halley step on [`erfc`]).
pub fn normal_quantile(p: f64) -> f64 {
    if !(p > 0.0 && p < 1.0) {
        return if p <= 0.0 { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] =
        [-5.447_609_879_822_406e1, 1.615_858_368_580_409e2, -1.556_989_798_598_866e2, 6.680_131_188_771_972e1, -1.328_068_155_288_572e1];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [7.784_695_709_041_462e-3, 3.224_671_290_700_398e-1, 2.445_134_137_142_996, 3.754_408_661_907_416];
    let low = 0.024_25;
    let x = if p < low {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5]) / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - low {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5]) / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };
    // One Halley step: e is the error of Φ(x).
    let e = 0.5 * erfc(-x / std::f64::consts::SQRT_2) - p;
    let u = e * (2.0 * std::f64::consts::PI).sqrt() * (x * x / 2.0).exp();
    x - u / (1.0 + x * u / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol * b.abs().max(1.0)
    }

    /// The normal distribution against known values.
    #[test]
    fn the_normal_distribution() {
        for (x, want) in [
            (0.0, 1.0),
            (0.5, 0.479_500_122_186_953_5),
            (1.0, 0.157_299_207_050_285_13),
            (2.0, 4.677_734_981_047_266e-3),
            (5.0, 1.537_459_794_428_034_8e-12),
        ] {
            assert!(close(erfc(x), want, 1e-13), "erfc({x}) = {}", erfc(x));
        }
        assert!(close(erfc(-1.0), 2.0 - 0.157_299_207_050_285_13, 1e-14));
        assert!(close(normal_quantile(0.975), 1.959_963_984_540_054, 1e-12));
        assert!(close(normal_quantile(0.5), 0.0, 1e-12));
        assert!(close(normal_quantile(1e-6), -4.753_424_308_822_899, 1e-10));
        assert!(close(p_value(1.959_963_984_540_054), 0.05, 1e-12));
    }

    /// Sen's slope, the tie-corrected variance and the plain test on a small series worked by hand.
    #[test]
    fn the_original_test_by_hand() {
        let t = [1.0, 2.0, 3.0, 4.0, 5.0];
        let y = [1.0, 3.0, 2.0, 5.0, 4.0];
        let r = trend(&t, &y, Test::Original, 0.05).unwrap();
        // Pairs: 8 up, 2 down.
        assert_eq!((r.n, r.s), (5, 6.0));
        assert!(close(r.var_s, 5.0 * 4.0 * 15.0 / 18.0, 1e-12));
        // Slopes −1, −1, 0.33, 0.5, 0.75, 1, 1, 1.33, 2, 3: median 0.875.
        assert!(close(r.slope, 0.875, 1e-12), "{}", r.slope);
        // The intercept: the median value less the slope times the median time.
        assert!(close(r.intercept, 3.0 - 0.875 * 3.0, 1e-12));
        assert!(close(r.z, 5.0 / (50.0f64 / 3.0).sqrt(), 1e-12));
        // Ties: two equal values take 2·1·9 out of 18·Var.
        let tied = trend(&t, &[1.0, 2.0, 2.0, 3.0, 4.0], Test::Original, 0.05).unwrap();
        assert!(close(tied.var_s, (5.0 * 4.0 * 15.0 - 18.0) / 18.0, 1e-12));
        // A level outside (0, 1), or too many values for the pairs, gives no trend.
        for alpha in [0.0, 1.0, f64::NAN] {
            assert!(trend(&t, &y, Test::HamedRao, alpha).is_none(), "{alpha}");
        }
        let seasons: Vec<usize> = (0..t.len()).map(|i| if i == 0 { usize::MAX } else { i % 2 }).collect();
        assert!(trend_seasonal(&t, &y, &seasons, 0.05).is_none(), "a season index out of range");
        let many: Vec<f64> = (0..=MAX_VALUES).map(|i| i as f64).collect();
        assert_eq!(sen_slope(&many, &many), None, "Sen's slope alone is bounded too");
        assert!(
            trend_seasonal(&[2000.0, 2000.5, 2001.0, 2001.5], &[1.0, 2.0, 3.0, 4.0], &[0, 1, 0], 0.05).is_none(),
            "seasons for every value"
        );
        // Seasons of one value give no pair: a series with too few in seasons of two or more has no
        // trend, whatever its length.
        assert!(trend_seasonal(&[2000.0, 2000.1, 2000.2, 2000.3, 2001.0], &[1.0, 2.0, 3.0, 4.0, 5.0], &[0, 1, 2, 3, 0], 0.05).is_none());
        // Values in any order are taken in time order: the same trend as sorted.
        let (t, y) = ([2000.0, 2001.0, 2002.0, 2003.0, 2004.0, 2005.0], [1.0, 3.0, 2.0, 5.0, 4.0, 6.0]);
        let sorted = trend(&t, &y, Test::Original, 0.05).unwrap();
        let shuffled =
            trend(&[2003.0, 2000.0, 2005.0, 2001.0, 2004.0, 2002.0], &[5.0, 1.0, 6.0, 3.0, 4.0, 2.0], Test::Original, 0.05).unwrap();
        assert_eq!((sorted.s, sorted.z, sorted.slope), (shuffled.s, shuffled.z, shuffled.slope));
        assert!(trend(&many, &many, Test::Original, 0.05).is_none());
        // Too few values, missing ones skipped.
        assert!(trend(&t, &[1.0, f64::NAN, 2.0, f64::NAN, 3.0], Test::Original, 0.05).is_none());
        assert_eq!(trend(&t, &[1.0, f64::NAN, 2.0, 4.0, 3.0], Test::Original, 0.05).unwrap().n, 4);
    }

    /// A clear trend is significant for every test; noise without one is not.
    #[test]
    fn trends_found_and_not() {
        let t: Vec<f64> = (0..40).map(|i| 2000.0 + f64::from(i) / 4.0).collect();
        // A pseudo-random noise, the same each run.
        let mut seed = 7u64;
        let mut noise = || {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            ((seed >> 33) as f64 / (1u64 << 31) as f64) - 0.5
        };
        let rising: Vec<f64> = t.iter().map(|t| 0.02 * (t - 2000.0) + 0.3 + 0.01 * noise()).collect();
        let flat: Vec<f64> = t.iter().map(|_| 0.5 + 0.1 * noise()).collect();
        for test in [Test::HamedRao, Test::YueWang, Test::Original, Test::Seasonal(4)] {
            let r = trend(&t, &rising, test, 0.05).unwrap();
            assert!(r.p < 0.01 && (r.slope - 0.02).abs() < 0.005, "{test:?}: {r:?}");
            let f = trend(&t, &flat, test, 0.05).unwrap();
            assert!(f.p > 0.05, "{test:?}: {f:?}");
        }
    }

    /// The same series through `pymannkendall` 1.4.3 (`original_test`, `hamed_rao_modification_test`,
    /// `yue_wang_modification_test(lag=1)`, `seasonal_test(period=12)`), with numpy's seeded noise:
    /// Sen's slope and intercept, S, Var(S), Z and p agree.
    #[test]
    #[expect(clippy::approx_constant, reason = "the series are seeded noise: a value near π/6 is a coincidence")]
    fn as_pymannkendall_gives() {
        let cases: &[(&str, &[f64], f64, f64, [f64; 4], [f64; 4], [f64; 4])] = &[
            (
                "ar_rising",
                &[
                    0.3996, 0.3929, 0.4141, 0.4304, 0.4602, 0.4764, 0.494, 0.4813, 0.4795, 0.4734, 0.4821, 0.5111, 0.5353, 0.5313, 0.5416,
                    0.548, 0.567, 0.5235, 0.52, 0.5476, 0.5885, 0.5805, 0.6253, 0.5655, 0.5931, 0.6149, 0.6348, 0.6666, 0.7021, 0.6683,
                ][..],
                0.008753333333333332,
                0.4063766666666667,
                [373.0, 3141.6666666666665, 6.636863533302587, 3.204281284752142e-11],
                [373.0, 2435.0322122618513, 7.538597992801056, 4.75175454539567e-14],
                [373.0, 5703.607377890534, 4.925701942158268, 8.405804681199669e-07],
            ),
            (
                "ar_flat",
                &[
                    0.4466, 0.5293, 0.4757, 0.5126, 0.5232, 0.4954, 0.5487, 0.5477, 0.4995, 0.541, 0.5504, 0.486, 0.4815, 0.4615, 0.3862,
                    0.4137, 0.458, 0.5231, 0.4883, 0.4986, 0.5049, 0.5061, 0.4801, 0.5197, 0.5571, 0.5834, 0.5123, 0.4638, 0.4183, 0.4101,
                    0.4064, 0.4043, 0.3711, 0.3286, 0.3564, 0.4254, 0.3774, 0.4244, 0.4869, 0.5272,
                ][..],
                -0.002545833333333321,
                0.5360937499999997,
                [-230.0, 7366.666666666667, -2.6680869144024206, 0.007628452120383988],
                [-230.0, 9218.727025114382, -2.385063821187819, 0.01707615864074641],
                [-230.0, 16316.61631700942, -1.7927528917461166, 0.07301244507305826],
            ),
            (
                "ties_falling",
                &[5.0, 4.9, 4.9, 5.2, 5.0, 4.4, 4.3, 4.8, 4.5, 4.3, 4.6, 4.0, 4.1, 4.4, 4.3, 4.4, 4.3, 4.1, 4.1, 4.0, 4.0, 3.7, 3.6, 3.9][..],
                -0.05313725490196082,
                4.911078431372549,
                [-197.0, 1603.6666666666667, -4.894395048339705, 9.860858256605098e-07],
                [-197.0, 1603.6666666666667, -4.894395048339705, 9.860858256605098e-07],
                [-197.0, 1948.515975874476, -4.440215881965339, 8.986868028193484e-06],
            ),
            (
                "short",
                &[3.1, 2.9, 3.4, 3.8, 3.6, 4.1][..],
                0.23333333333333325,
                2.916666666666667,
                [11.0, 28.333333333333332, 1.8786728732554487, 0.060289173990602185],
                [11.0, 28.333333333333332, 1.8786728732554487, 0.060289173990602185],
                [11.0, 3.074935400516776, 5.7027193866927695, 1.1791100540392563e-08],
            ),
        ];
        let same = |what: &str, got: f64, want: f64| {
            assert!((got - want).abs() <= 1e-9 * want.abs().max(1e-300) + 1e-12, "{what}: {got} for {want}")
        };
        for (name, x, slope, intercept, original, hamed_rao, yue_wang) in cases {
            // pymannkendall's times are the values' places, from 0.
            let t: Vec<f64> = (0..x.len()).map(|i| i as f64).collect();
            for (test, want) in [(Test::Original, original), (Test::HamedRao, hamed_rao), (Test::YueWang, yue_wang)] {
                let r = trend(&t, x, test, 0.05).unwrap();
                let what = |k: &str| format!("{name} {test:?} {k}");
                same(&what("slope"), r.slope, *slope);
                same(&what("intercept"), r.intercept, *intercept);
                same(&what("S"), r.s, want[0]);
                same(&what("Var(S)"), r.var_s, want[1]);
                same(&what("Z"), r.z, want[2]);
                // p to a part in a million, or 1e-14 where pymannkendall's 2·(1 − Φ(|z|)) has lost
                // its digits to the subtraction (erfc here keeps them).
                assert!((r.p - want[3]).abs() <= 1e-6 * want[3] + 1e-14, "{} p: {} for {}", what(""), r.p, want[3]);
            }
        }
        // Monthly over four years, with a yearly cycle: the seasonal test by month.
        let x = &[
            9.783, 11.242, 11.884, 12.936, 12.886, 11.309, 10.47, 8.344, 7.923, 7.204, 7.211, 9.4, 10.11, 10.936, 13.174, 13.622, 12.772,
            12.199, 10.743, 9.111, 8.012, 9.202, 8.143, 9.333, 11.416, 12.09, 14.53, 14.224, 13.738, 12.435, 11.279, 9.175, 8.785, 7.257,
            9.35, 9.554, 11.774, 13.653, 13.892, 13.604, 14.123, 13.6, 12.068, 10.363, 9.239, 8.46, 9.288, 10.121,
        ][..];
        let t: Vec<f64> = (0..x.len()).map(|k| 2000.0 + (k as f64 + 0.5) / 12.0).collect();
        let r = trend(&t, x, Test::Seasonal(12), 0.05).unwrap();
        let want = [54.0, 104.00000000000001, 5.197077581161876, 2.0244588982976097e-07];
        same("seasonal slope", r.slope, 0.56975);
        same("seasonal S", r.s, want[0]);
        same("seasonal Var(S)", r.var_s, want[1]);
        same("seasonal Z", r.z, want[2]);
        assert!((r.p - want[3]).abs() <= 1e-6 * want[3] + 1e-14, "seasonal p: {} for {}", r.p, want[3]);
    }

    /// Parsing the tests' names.
    #[test]
    fn test_names() {
        assert_eq!(Test::parse("", 12), Some(Test::HamedRao));
        assert_eq!(Test::parse("Hamed-Rao", 12), Some(Test::HamedRao));
        assert_eq!(Test::parse("yueWang", 12), Some(Test::YueWang));
        assert_eq!(Test::parse("seasonal", 23), Some(Test::Seasonal(23)));
        assert_eq!(Test::parse("original", 12), Some(Test::Original));
        assert_eq!(Test::parse("ols", 12), None);
    }
}
