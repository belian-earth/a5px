//! Reducers that composite per-source cell values.
//!
//! With `composite`, a read finalises `stat` for each source and cell, then
//! reduces the sources' values per cell and band. Each value carries the
//! weight of the source in that cell: its valid pixel count (fractional
//! under overlay sampling), or 1 for a centroid sample. Sources with no
//! valid pixels in a cell, and NaN values, do not take part.

use crate::error::{A5CogError, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reducer {
    Mean,
    Median,
    Min,
    Max,
    Sum,
    Count,
    Var,
    Sd,
    Majority,
}

impl Reducer {
    /// Parse the extendr argument: empty means no composite.
    pub fn parse(s: &str) -> Result<Option<Self>> {
        Ok(Some(match s {
            "" => return Ok(None),
            "mean" => Reducer::Mean,
            "median" => Reducer::Median,
            "min" => Reducer::Min,
            "max" => Reducer::Max,
            "sum" => Reducer::Sum,
            "count" => Reducer::Count,
            "var" => Reducer::Var,
            "sd" => Reducer::Sd,
            "majority" => Reducer::Majority,
            other => {
                return Err(A5CogError::Invalid(format!("unknown composite {other:?}")));
            }
        }))
    }

    /// Reduce `(value, weight)` pairs, all with a positive weight and a
    /// finite value. NaN when there are none (or, for `var` / `sd`, fewer
    /// than two). Reorders `vals`.
    pub fn reduce(self, vals: &mut [(f64, f64)]) -> f64 {
        if vals.is_empty() {
            return match self {
                Reducer::Count => 0.0,
                _ => f64::NAN,
            };
        }
        match self {
            Reducer::Count => vals.len() as f64,
            Reducer::Sum => vals.iter().map(|&(v, _)| v).sum(),
            Reducer::Min => vals.iter().map(|&(v, _)| v).fold(f64::INFINITY, f64::min),
            Reducer::Max => vals.iter().map(|&(v, _)| v).fold(f64::NEG_INFINITY, f64::max),
            Reducer::Mean => {
                let (sv, sw) = vals.iter().fold((0.0, 0.0), |(sv, sw), &(v, w)| (sv + w * v, sw + w));
                sv / sw
            }
            Reducer::Median => weighted_median(vals),
            Reducer::Var | Reducer::Sd => {
                let var = weighted_var(vals);
                if self == Reducer::Sd { var.sqrt() } else { var }
            }
            Reducer::Majority => {
                // total weight per distinct value; ties go to the smallest
                vals.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut best = (f64::NAN, f64::NEG_INFINITY);
                let mut i = 0;
                while i < vals.len() {
                    let v = vals[i].0;
                    let mut w = 0.0;
                    while i < vals.len() && vals[i].0 == v {
                        w += vals[i].1;
                        i += 1;
                    }
                    if w > best.1 {
                        best = (v, w);
                    }
                }
                best.0
            }
        }
    }
}

/// The value at which the cumulative weight first reaches half the total;
/// when it lands exactly on half, the mean of that value and the next, so
/// equal weights give the ordinary median.
fn weighted_median(vals: &mut [(f64, f64)]) -> f64 {
    vals.sort_by(|a, b| a.0.total_cmp(&b.0));
    let half = vals.iter().map(|&(_, w)| w).sum::<f64>() * 0.5;
    let mut cum = 0.0;
    for k in 0..vals.len() {
        cum += vals[k].1;
        if cum >= half {
            let exact = (cum - half).abs() <= 1e-12 * half.max(1.0);
            return if exact && k + 1 < vals.len() {
                0.5 * (vals[k].0 + vals[k + 1].0)
            } else {
                vals[k].0
            };
        }
    }
    vals[vals.len() - 1].0
}

/// Weighted variance with reliability weights, which equals the sample
/// variance (divisor n - 1) when the weights are equal. NaN for fewer than
/// two values.
fn weighted_var(vals: &[(f64, f64)]) -> f64 {
    if vals.len() < 2 {
        return f64::NAN;
    }
    let sw: f64 = vals.iter().map(|&(_, w)| w).sum();
    let sw2: f64 = vals.iter().map(|&(_, w)| w * w).sum();
    let mean = vals.iter().map(|&(v, w)| w * v).sum::<f64>() / sw;
    let ss: f64 = vals.iter().map(|&(v, w)| w * (v - mean) * (v - mean)).sum();
    let denom = sw - sw2 / sw;
    if denom > 0.0 { ss / denom } else { f64::NAN }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(red: Reducer, v: &[(f64, f64)]) -> f64 {
        red.reduce(&mut v.to_vec())
    }

    #[test]
    fn equal_weights_match_ordinary_statistics() {
        let v = [(3.0, 1.0), (1.0, 1.0), (2.0, 1.0), (10.0, 1.0)];
        assert_eq!(r(Reducer::Median, &v), 2.5);
        assert_eq!(r(Reducer::Mean, &v), 4.0);
        // sample variance of 3, 1, 2, 10
        assert!((r(Reducer::Var, &v) - 50.0 / 3.0).abs() < 1e-12);
        assert_eq!(r(Reducer::Count, &v), 4.0);
        assert_eq!(r(Reducer::Sum, &v), 16.0);
        assert_eq!(r(Reducer::Min, &v), 1.0);
        assert_eq!(r(Reducer::Max, &v), 10.0);
    }

    #[test]
    fn weights_shift_mean_median_and_majority() {
        let v = [(1.0, 9.0), (5.0, 1.0)];
        assert_eq!(r(Reducer::Mean, &v), 1.4);
        assert_eq!(r(Reducer::Median, &v), 1.0);
        let c = [(2.0, 1.0), (7.0, 3.0), (2.0, 1.5)];
        assert_eq!(r(Reducer::Majority, &c), 7.0);
        let tie = [(4.0, 1.0), (3.0, 1.0)];
        assert_eq!(r(Reducer::Majority, &tie), 3.0);
    }

    #[test]
    fn empty_and_single_inputs() {
        assert!(r(Reducer::Mean, &[]).is_nan());
        assert_eq!(r(Reducer::Count, &[]), 0.0);
        assert!(r(Reducer::Sd, &[(1.0, 4.0)]).is_nan());
    }
}
