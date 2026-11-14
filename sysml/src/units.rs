//! Physical dimensions and the unit symbols written in `value [unit]`.
//!
//! Values are kept in SI base units; a [`Dim`] records the exponent of each
//! base quantity so that `kg + W` can be reported and `m / s^2` worked out.

use std::fmt;

/// Exponents of the SI base quantities: length, mass, time, electric current,
/// temperature, amount of substance, luminous intensity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Dim(pub [i8; 7]);

const BASE: [&str; 7] = ["m", "kg", "s", "A", "K", "mol", "cd"];

impl Dim {
    pub const NONE: Dim = Dim([0; 7]);
    pub const LENGTH: Dim = Dim([1, 0, 0, 0, 0, 0, 0]);
    pub const MASS: Dim = Dim([0, 1, 0, 0, 0, 0, 0]);
    pub const TIME: Dim = Dim([0, 0, 1, 0, 0, 0, 0]);
    pub const CURRENT: Dim = Dim([0, 0, 0, 1, 0, 0, 0]);
    pub const TEMPERATURE: Dim = Dim([0, 0, 0, 0, 1, 0, 0]);
    pub const AMOUNT: Dim = Dim([0, 0, 0, 0, 0, 1, 0]);
    pub const LUMINOSITY: Dim = Dim([0, 0, 0, 0, 0, 0, 1]);

    pub fn is_none(self) -> bool {
        self == Dim::NONE
    }

    pub fn pow(self, n: i8) -> Dim {
        Dim(std::array::from_fn(|i| self.0[i].saturating_mul(n)))
    }

    const fn of(e: [i8; 7]) -> Dim {
        Dim(e)
    }
}

impl std::ops::Mul for Dim {
    type Output = Dim;
    fn mul(self, other: Dim) -> Dim {
        Dim(std::array::from_fn(|i| self.0[i].saturating_add(other.0[i])))
    }
}

impl std::ops::Div for Dim {
    type Output = Dim;
    fn div(self, other: Dim) -> Dim {
        Dim(std::array::from_fn(|i| self.0[i].saturating_sub(other.0[i])))
    }
}

/// Named coherent units, preferred when showing a dimension.
const DERIVED: [(&str, Dim); 9] = [
    ("N", Dim::of([1, 1, -2, 0, 0, 0, 0])),
    ("J", Dim::of([2, 1, -2, 0, 0, 0, 0])),
    ("W", Dim::of([2, 1, -3, 0, 0, 0, 0])),
    ("Pa", Dim::of([-1, 1, -2, 0, 0, 0, 0])),
    ("Hz", Dim::of([0, 0, -1, 0, 0, 0, 0])),
    ("C", Dim::of([0, 0, 1, 1, 0, 0, 0])),
    ("V", Dim::of([2, 1, -3, -1, 0, 0, 0])),
    ("Ohm", Dim::of([2, 1, -3, -2, 0, 0, 0])),
    ("F", Dim::of([-2, -1, 4, 2, 0, 0, 0])),
];

impl fmt::Display for Dim {
    /// The SI unit of this dimension, e.g. `W`, `m/s^2`, `kg*m^2`; empty if
    /// dimensionless.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some((name, _)) = DERIVED.iter().find(|(_, d)| d == self) {
            return f.write_str(name);
        }
        let part = |e: i8, sym: &str| if e == 1 { sym.to_string() } else { format!("{sym}^{e}") };
        // Mass first: `kg*m^2` reads more naturally than `m^2*kg`.
        let order = [1, 0, 2, 3, 4, 5, 6];
        let num: Vec<String> = order.iter().filter(|&&i| self.0[i] > 0).map(|&i| part(self.0[i], BASE[i])).collect();
        let den: Vec<String> = order.iter().filter(|&&i| self.0[i] < 0).map(|&i| part(-self.0[i], BASE[i])).collect();
        match (num.is_empty(), den.is_empty()) {
            (true, true) => Ok(()),
            (false, true) => f.write_str(&num.join("*")),
            (true, false) => write!(f, "1/{}", den.join("*")),
            (false, false) => write!(f, "{}/{}", num.join("*"), den.join("*")),
        }
    }
}

/// A unit: how many SI base units one of it is, and its dimension.
#[derive(Clone, Debug, PartialEq)]
pub struct Unit {
    /// As written, e.g. `km/h`.
    pub name: String,
    pub scale: f64,
    pub dim: Dim,
}

/// Symbols that take SI prefixes (`k` + `m` = km).
const PREFIXABLE: &[(&str, f64, Dim)] = &[
    ("m", 1.0, Dim::LENGTH),
    ("g", 1e-3, Dim::MASS),
    ("s", 1.0, Dim::TIME),
    ("A", 1.0, Dim::CURRENT),
    ("K", 1.0, Dim::TEMPERATURE),
    ("mol", 1.0, Dim::AMOUNT),
    ("cd", 1.0, Dim::LUMINOSITY),
    ("N", 1.0, DERIVED[0].1),
    ("J", 1.0, DERIVED[1].1),
    ("W", 1.0, DERIVED[2].1),
    ("Pa", 1.0, DERIVED[3].1),
    ("Hz", 1.0, DERIVED[4].1),
    ("C", 1.0, DERIVED[5].1),
    ("V", 1.0, DERIVED[6].1),
    ("Ohm", 1.0, DERIVED[7].1),
    ("F", 1.0, DERIVED[8].1),
    ("Wh", 3600.0, DERIVED[1].1),
    ("Ah", 3600.0, DERIVED[5].1),
    ("L", 1e-3, Dim::of([3, 0, 0, 0, 0, 0, 0])),
    ("l", 1e-3, Dim::of([3, 0, 0, 0, 0, 0, 0])),
    ("bar", 1e5, DERIVED[3].1),
];

/// Symbols and names used as written.
const PLAIN: &[(&str, f64, Dim)] = &[
    ("one", 1.0, Dim::NONE),
    ("rad", 1.0, Dim::NONE),
    ("sr", 1.0, Dim::NONE),
    ("deg", std::f64::consts::PI / 180.0, Dim::NONE),
    ("percent", 0.01, Dim::NONE),
    ("min", 60.0, Dim::TIME),
    ("h", 3600.0, Dim::TIME),
    ("d", 86400.0, Dim::TIME),
    ("t", 1000.0, Dim::MASS),
    ("rpm", 1.0 / 60.0, DERIVED[4].1),
    ("Ω", 1.0, DERIVED[7].1),
    ("degC", 1.0, Dim::TEMPERATURE),
    ("metre", 1.0, Dim::LENGTH),
    ("meter", 1.0, Dim::LENGTH),
    ("kilogram", 1.0, Dim::MASS),
    ("gram", 1e-3, Dim::MASS),
    ("second", 1.0, Dim::TIME),
    ("minute", 60.0, Dim::TIME),
    ("hour", 3600.0, Dim::TIME),
    ("ampere", 1.0, Dim::CURRENT),
    ("kelvin", 1.0, Dim::TEMPERATURE),
    ("newton", 1.0, DERIVED[0].1),
    ("joule", 1.0, DERIVED[1].1),
    ("watt", 1.0, DERIVED[2].1),
    ("pascal", 1.0, DERIVED[3].1),
    ("hertz", 1.0, DERIVED[4].1),
    ("volt", 1.0, DERIVED[6].1),
    ("ohm", 1.0, DERIVED[7].1),
];

const PREFIXES: &[(&str, f64)] = &[
    ("G", 1e9),
    ("M", 1e6),
    ("k", 1e3),
    ("h", 1e2),
    ("c", 1e-2),
    ("m", 1e-3),
    ("u", 1e-6),
    ("µ", 1e-6),
    ("μ", 1e-6),
    ("n", 1e-9),
];

/// Looks up a single unit symbol such as `kg`, `mAh` or `km`.
pub fn lookup(symbol: &str) -> Option<Unit> {
    let unit = |scale, dim| Some(Unit { name: symbol.to_string(), scale, dim });
    if let Some(&(_, scale, dim)) = PLAIN.iter().chain(PREFIXABLE).find(|(s, ..)| *s == symbol) {
        return unit(scale, dim);
    }
    for &(prefix, factor) in PREFIXES {
        if let Some(rest) = symbol.strip_prefix(prefix)
            && let Some(&(_, scale, dim)) = PREFIXABLE.iter().find(|(s, ..)| *s == rest)
        {
            return unit(factor * scale, dim);
        }
    }
    None
}

/// `v` with up to six significant digits, without trailing zeros.
pub fn format_number(v: f64) -> String {
    if !v.is_finite() {
        return v.to_string();
    }
    if v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let magnitude = v.abs().log10().floor() as i32;
    if !(-4..15).contains(&magnitude) {
        return format!("{v:.5e}").replace("0e", "e").trim_end_matches('.').to_string();
    }
    let decimals = (5 - magnitude).max(0) as usize;
    let s = format!("{v:.decimals$}");
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.') } else { &s };
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_and_derived_units() {
        assert_eq!(lookup("kg").unwrap().scale, 1.0);
        assert_eq!(lookup("km").unwrap().scale, 1000.0);
        assert_eq!(lookup("mAh").unwrap().dim, lookup("C").unwrap().dim);
        assert_eq!(lookup("min").unwrap().scale, 60.0);
        assert!(lookup("furlong").is_none());
        assert_eq!(DERIVED[2].1.to_string(), "W");
        assert_eq!((Dim::LENGTH / Dim::TIME.pow(2)).to_string(), "m/s^2");
        assert_eq!((Dim::MASS * Dim::LENGTH.pow(2)).to_string(), "kg*m^2");
    }

    #[test]
    fn numbers() {
        assert_eq!(format_number(6.0), "6");
        assert_eq!(format_number(0.1 + 0.2), "0.3");
        assert_eq!(format_number(1.23456789), "1.23457");
        assert_eq!(format_number(-2.5), "-2.5");
    }
}
