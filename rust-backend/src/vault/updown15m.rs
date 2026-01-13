use statrs::distribution::{ContinuousCDF, Normal};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpDownAsset {
    Btc,
    Eth,
    Sol,
    Xrp,
}

impl UpDownAsset {
    pub fn as_str(&self) -> &'static str {
        match self {
            UpDownAsset::Btc => "btc",
            UpDownAsset::Eth => "eth",
            UpDownAsset::Sol => "sol",
            UpDownAsset::Xrp => "xrp",
        }
    }

    pub fn binance_symbol(&self) -> &'static str {
        match self {
            UpDownAsset::Btc => "BTCUSDT",
            UpDownAsset::Eth => "ETHUSDT",
            UpDownAsset::Sol => "SOLUSDT",
            UpDownAsset::Xrp => "XRPUSDT",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct UpDown15mMarket {
    pub asset: UpDownAsset,
    pub start_ts: i64,
    pub end_ts: i64,
}

pub fn parse_updown_15m_slug(slug: &str) -> Option<UpDown15mMarket> {
    let lower = slug.to_ascii_lowercase();

    for (asset, prefix) in [
        (UpDownAsset::Btc, "btc-updown-15m-"),
        (UpDownAsset::Eth, "eth-updown-15m-"),
        (UpDownAsset::Sol, "sol-updown-15m-"),
        (UpDownAsset::Xrp, "xrp-updown-15m-"),
    ] {
        if lower.starts_with(prefix) {
            let rest = &lower[prefix.len()..];
            let ts_str = rest.split('-').next().unwrap_or("");
            let start_ts = ts_str.parse::<i64>().ok()?;
            let end_ts = start_ts + 15 * 60;
            return Some(UpDown15mMarket {
                asset,
                start_ts,
                end_ts,
            });
        }
    }

    None
}

#[derive(Debug, Clone, Copy)]
pub struct UpDown15mParams {
    pub shrink_to_half: f64,
}

impl Default for UpDown15mParams {
    fn default() -> Self {
        Self {
            shrink_to_half: 0.35,
        }
    }
}

pub fn p_up_driftless_lognormal(
    p_start: f64,
    p_now: f64,
    sigma_per_sqrt_s: f64,
    t_rem_sec: f64,
) -> Option<f64> {
    if !(p_start > 0.0 && p_now > 0.0) {
        return None;
    }
    if !(sigma_per_sqrt_s.is_finite() && sigma_per_sqrt_s > 0.0) {
        return None;
    }
    if !(t_rem_sec.is_finite() && t_rem_sec > 0.0) {
        return None;
    }

    let x = (p_now / p_start).ln();
    let denom = sigma_per_sqrt_s * t_rem_sec.sqrt();
    if !(denom.is_finite() && denom > 0.0) {
        return None;
    }

    let z = x / denom;
    let n = Normal::new(0.0, 1.0).ok()?;
    let p = n.cdf(z);
    if p.is_finite() {
        Some(p.clamp(0.0001, 0.9999))
    } else {
        None
    }
}

pub fn shrink_to_half(p: f64, shrink: f64) -> f64 {
    let s = shrink.clamp(0.0, 1.0);
    (0.5 + s * (p - 0.5)).clamp(0.0001, 0.9999)
}
