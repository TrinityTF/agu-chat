//! The sound of a highlight alert, shared by the desktop and Android apps: the built-in ping.

/// Two short sine blips a fifth apart: audible over stream audio without being harsh (the same envelope as the
/// Web Audio one in the website's chat).
pub fn ping_samples(rate: u32) -> Vec<f32> {
    let total = (rate as f32 * 0.5) as usize;
    let mut out = vec![0.0f32; total];
    for (freq, at) in [(988.0f32, 0.0f32), (1480.0, 0.09)] {
        let start = (at * rate as f32) as usize;
        let len = (0.34 * rate as f32) as usize;
        for i in 0..len {
            if start + i >= total {
                break;
            }
            let t = i as f32 / rate as f32;
            // exponential ramp 0.0001 -> 0.5 over 12 ms, then down to 0.0001 at 0.32 s
            let env = if t < 0.012 {
                0.0001 * (0.5f32 / 0.0001).powf(t / 0.012)
            } else if t < 0.32 {
                0.5 * (0.0001f32 / 0.5).powf((t - 0.012) / (0.32 - 0.012))
            } else {
                0.0
            };
            out[start + i] += env * (2.0 * std::f32::consts::PI * freq * t).sin();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_has_two_blips_and_stays_in_range() {
        let s = ping_samples(44100);
        assert!(s.iter().all(|v| v.abs() <= 1.0));
        assert!(s[..2000].iter().any(|v| v.abs() > 0.1));
        assert!(s[s.len() - 100..].iter().all(|v| v.abs() < 0.01));
    }
}
