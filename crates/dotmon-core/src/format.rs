//! Human-readable formatting (French units: o, Ko, Mo, Go, To — base 1024).

const UNITS: &[&str] = &["o", "Ko", "Mo", "Go", "To", "Po"];

/// Splits a byte count into a value and a unit, e.g. `(11.4, "Go")`.
pub fn bytes_parts(bytes: u64) -> (f64, &'static str) {
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    (v, UNITS[i])
}

fn number(v: f64, unit_index_is_bytes: bool) -> String {
    if unit_index_is_bytes || v >= 100.0 {
        format!("{v:.0}")
    } else if v >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

/// `1536` → `"1.50 Ko"`, `12_884_901_888` → `"12.0 Go"`.
pub fn bytes(bytes: u64) -> String {
    let (v, unit) = bytes_parts(bytes);
    format!("{} {unit}", number(v, unit == "o"))
}

/// Value only, without unit (for big dot-matrix numbers).
pub fn bytes_value(bytes: u64) -> String {
    let (v, unit) = bytes_parts(bytes);
    number(v, unit == "o")
}

pub fn bytes_unit(bytes: u64) -> &'static str {
    bytes_parts(bytes).1
}

pub fn rate(bps: u64) -> String {
    format!("{}/s", self::bytes(bps))
}

/// Percentage with adaptive precision: `"37"`, `"4.2"`, `"0.0"`.
pub fn percent(p: f32) -> String {
    let p = if p.is_finite() { p.clamp(0.0, 100.0) } else { 0.0 };
    if p >= 10.0 { format!("{p:.0}") } else { format!("{p:.1}") }
}

/// `"3 j 04:12"`, `"04:12:09"`.
pub fn uptime(seconds: u64) -> String {
    let d = seconds / 86_400;
    let h = (seconds % 86_400) / 3600;
    let m = (seconds % 3600) / 60;
    let s = seconds % 60;
    if d > 0 { format!("{d} j {h:02}:{m:02}") } else { format!("{h:02}:{m:02}:{s:02}") }
}

/// Pads with leading zeros for the dot-matrix look (`7` → `"007"`).
pub fn padded(v: u32, width: usize) -> String {
    format!("{v:0width$}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_formatting() {
        assert_eq!(bytes(0), "0 o");
        assert_eq!(bytes(1023), "1023 o");
        assert_eq!(bytes(1536), "1.50 Ko");
        assert_eq!(bytes(12 * 1024 * 1024 * 1024), "12.0 Go");
        assert_eq!(bytes(300 * 1024 * 1024), "300 Mo");
        assert_eq!(bytes_value(32 * 1024 * 1024 * 1024), "32.0");
        assert_eq!(bytes_unit(32 * 1024 * 1024 * 1024), "Go");
    }

    #[test]
    fn rate_formatting() {
        assert_eq!(rate(2048), "2.00 Ko/s");
    }

    #[test]
    fn percent_formatting() {
        assert_eq!(percent(37.4), "37");
        assert_eq!(percent(4.24), "4.2");
        assert_eq!(percent(-3.0), "0.0");
        assert_eq!(percent(250.0), "100");
        assert_eq!(percent(f32::NAN), "0.0");
    }

    #[test]
    fn uptime_formatting() {
        assert_eq!(uptime(59), "00:00:59");
        assert_eq!(uptime(3 * 86_400 + 4 * 3600 + 12 * 60 + 5), "3 j 04:12");
    }

    #[test]
    fn padded_formatting() {
        assert_eq!(padded(7, 3), "007");
        assert_eq!(padded(1234, 3), "1234");
    }
}
