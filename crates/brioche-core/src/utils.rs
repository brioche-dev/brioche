use std::fmt::Write as _;

pub mod output_buffer;
pub mod zstd;

pub fn is_default<T>(value: &T) -> bool
where
    T: Default + PartialEq,
{
    *value == Default::default()
}

const SECS_PRECISION: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DisplayDuration(pub std::time::Duration);

impl std::fmt::Display for DisplayDuration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let max_width = f.precision().unwrap_or(usize::MAX);

        let secs = self.0.as_secs_f64();

        let mut buffer = String::new();
        let mut unaligned = String::new();

        if secs < 60.0 {
            // Write the integer part of the second
            write!(&mut buffer, "{secs:.0}")?;

            // Compute how much room we have for the fractional part
            let secs_integer_width = buffer.len();
            let secs_fractional_width = max_width
                .saturating_sub(secs_integer_width)
                .saturating_sub(2);

            let secs_precision = std::cmp::min(secs_fractional_width, SECS_PRECISION);
            write!(&mut unaligned, "{secs:.0$}s", secs_precision)?;
        } else {
            // Use the rounded seconds, and break them up into hours,
            // minutes, and days
            let secs = secs.floor() as u64;
            let (secs, mins) = split_unit(secs, 60);
            let (mins, hours) = split_unit(mins, 60);

            let units = [(hours, "h"), (mins, "m"), (secs, "s")]
                .into_iter()
                .skip_while(|&(count, _)| count == 0);
            for (count, unit) in units {
                let is_first = unaligned.is_empty();

                buffer.clear();
                if is_first {
                    write!(&mut buffer, "{count}{unit}")?;
                } else {
                    // For successive units, use a zero-padded count
                    // for alignment
                    write!(&mut buffer, "{count:2}{unit}")?;
                }

                let within_width = unaligned.len() + buffer.len() <= max_width;
                if unaligned.is_empty() || within_width {
                    // This is either the first unit, or we're still within
                    // the width, so add this unit
                    unaligned += &buffer;
                } else {
                    // We've exceeded the width, so bail early
                    break;
                }
            }
        }

        let needed_fill = f
            .width()
            .unwrap_or_default()
            .saturating_sub(unaligned.len());
        let (left_fill, right_fill) = match f.align() {
            Some(std::fmt::Alignment::Left) | None => (0, needed_fill),
            Some(std::fmt::Alignment::Right) => (needed_fill, 0),
            Some(std::fmt::Alignment::Center) => {
                let half_fill = needed_fill / 2;
                (half_fill, needed_fill.saturating_sub(half_fill))
            }
        };

        for _ in 0..left_fill {
            f.write_char(f.fill())?;
        }
        f.write_str(&unaligned)?;
        for _ in 0..right_fill {
            f.write_char(f.fill())?;
        }

        Ok(())
    }
}

fn split_unit(little_unit: u64, big_unit_size: u64) -> (u64, u64) {
    let big_count = little_unit / big_unit_size;
    let little_remainder = little_unit - (big_count * big_unit_size);

    (little_remainder, big_count)
}

#[cfg(test)]
mod tests {
    use super::DisplayDuration;

    fn display_duration_ms(ms: u64) -> DisplayDuration {
        DisplayDuration(std::time::Duration::from_millis(ms))
    }

    #[test]
    fn test_display_duration_basic() {
        assert_eq!(display_duration_ms(0).to_string(), "0.00s",);
        assert_eq!(display_duration_ms(10).to_string(), "0.01s",);
        assert_eq!(display_duration_ms(99).to_string(), "0.10s",);
        assert_eq!(display_duration_ms(1_010).to_string(), "1.01s");
        assert_eq!(display_duration_ms(59_990).to_string(), "59.99s");
        assert_eq!(display_duration_ms(59_999).to_string(), "60.00s");

        assert_eq!(display_duration_ms(60_000).to_string(), "1m 0s");
        assert_eq!(display_duration_ms(60_999).to_string(), "1m 0s");
        assert_eq!(display_duration_ms(61_000).to_string(), "1m 1s");
        assert_eq!(display_duration_ms(120_000).to_string(), "2m 0s");
        assert_eq!(display_duration_ms(120_000).to_string(), "2m 0s");

        assert_eq!(display_duration_ms(3_599_999).to_string(), "59m59s");
        assert_eq!(display_duration_ms(3_600_000).to_string(), "1h 0m 0s");
        assert_eq!(display_duration_ms(36_000_000).to_string(), "10h 0m 0s");
        assert_eq!(display_duration_ms(360_000_000).to_string(), "100h 0m 0s");
    }

    #[test]
    fn test_display_duration_with_precision() {
        assert_eq!(format!("{:.0}", display_duration_ms(2500)), "2s");
        assert_eq!(format!("{:.1}", display_duration_ms(2500)), "2s");
        assert_eq!(format!("{:.2}", display_duration_ms(2500)), "2s");
        assert_eq!(format!("{:.3}", display_duration_ms(2500)), "2s");
        assert_eq!(format!("{:.4}", display_duration_ms(2500)), "2.5s");
        assert_eq!(format!("{:.5}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:.6}", display_duration_ms(2500)), "2.50s");

        assert_eq!(format!("{:.0}", display_duration_ms(65_000)), "1m");
        assert_eq!(format!("{:.1}", display_duration_ms(65_000)), "1m");
        assert_eq!(format!("{:.2}", display_duration_ms(65_000)), "1m");
        assert_eq!(format!("{:.3}", display_duration_ms(65_000)), "1m");
        assert_eq!(format!("{:.4}", display_duration_ms(65_000)), "1m");
        assert_eq!(format!("{:.5}", display_duration_ms(65_000)), "1m 5s");
        assert_eq!(format!("{:.6}", display_duration_ms(65_000)), "1m 5s");

        assert_eq!(format!("{:.0}", display_duration_ms(605_000)), "10m");
        assert_eq!(format!("{:.1}", display_duration_ms(605_000)), "10m");
        assert_eq!(format!("{:.2}", display_duration_ms(605_000)), "10m");
        assert_eq!(format!("{:.3}", display_duration_ms(605_000)), "10m");
        assert_eq!(format!("{:.4}", display_duration_ms(605_000)), "10m");
        assert_eq!(format!("{:.5}", display_duration_ms(605_000)), "10m");
        assert_eq!(format!("{:.6}", display_duration_ms(605_000)), "10m 5s");
        assert_eq!(format!("{:.7}", display_duration_ms(605_000)), "10m 5s");

        assert_eq!(format!("{:.0}", display_duration_ms(3_661_000)), "1h");
        assert_eq!(format!("{:.1}", display_duration_ms(3_661_000)), "1h");
        assert_eq!(format!("{:.2}", display_duration_ms(3_661_000)), "1h");
        assert_eq!(format!("{:.3}", display_duration_ms(3_661_000)), "1h");
        assert_eq!(format!("{:.4}", display_duration_ms(3_661_000)), "1h");
        assert_eq!(format!("{:.5}", display_duration_ms(3_661_000)), "1h 1m");
        assert_eq!(format!("{:.6}", display_duration_ms(3_661_000)), "1h 1m");
        assert_eq!(format!("{:.7}", display_duration_ms(3_661_000)), "1h 1m");
        assert_eq!(format!("{:.8}", display_duration_ms(3_661_000)), "1h 1m 1s");
        assert_eq!(format!("{:.9}", display_duration_ms(3_661_000)), "1h 1m 1s");

        assert_eq!(format!("{:.0}", display_duration_ms(36_061_000)), "10h");
        assert_eq!(format!("{:.1}", display_duration_ms(36_061_000)), "10h");
        assert_eq!(format!("{:.2}", display_duration_ms(36_061_000)), "10h");
        assert_eq!(format!("{:.3}", display_duration_ms(36_061_000)), "10h");
        assert_eq!(format!("{:.4}", display_duration_ms(36_061_000)), "10h");
        assert_eq!(format!("{:.5}", display_duration_ms(36_061_000)), "10h");
        assert_eq!(format!("{:.6}", display_duration_ms(36_061_000)), "10h 1m");
        assert_eq!(format!("{:.7}", display_duration_ms(36_061_000)), "10h 1m");
        assert_eq!(format!("{:.8}", display_duration_ms(36_061_000)), "10h 1m");
        assert_eq!(
            format!("{:.9}", display_duration_ms(36_061_000)),
            "10h 1m 1s"
        );
        assert_eq!(
            format!("{:.10}", display_duration_ms(36_061_000)),
            "10h 1m 1s"
        );
    }

    #[test]
    fn test_display_duration_with_padding() {
        assert_eq!(format!("{:0}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:1}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:2}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:3}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:4}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:5}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:6}", display_duration_ms(2500)), "2.50s ");
        assert_eq!(format!("{:7}", display_duration_ms(2500)), "2.50s  ");
        assert_eq!(format!("{:8}", display_duration_ms(2500)), "2.50s   ");

        assert_eq!(format!("{:<0}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:<1}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:<2}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:<3}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:<4}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:<5}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:<6}", display_duration_ms(2500)), "2.50s ");
        assert_eq!(format!("{:<7}", display_duration_ms(2500)), "2.50s  ");
        assert_eq!(format!("{:<8}", display_duration_ms(2500)), "2.50s   ");

        assert_eq!(format!("{:>0}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:>1}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:>2}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:>3}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:>4}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:>5}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:>6}", display_duration_ms(2500)), " 2.50s");
        assert_eq!(format!("{:>7}", display_duration_ms(2500)), "  2.50s");
        assert_eq!(format!("{:>8}", display_duration_ms(2500)), "   2.50s");

        assert_eq!(format!("{:^0}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:^1}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:^2}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:^3}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:^4}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:^5}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:^6}", display_duration_ms(2500)), "2.50s ");
        assert_eq!(format!("{:^7}", display_duration_ms(2500)), " 2.50s ");
        assert_eq!(format!("{:^8}", display_duration_ms(2500)), " 2.50s  ");

        assert_eq!(format!("{:-^0}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:-^1}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:-^2}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:-^3}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:-^4}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:-^5}", display_duration_ms(2500)), "2.50s");
        assert_eq!(format!("{:-^6}", display_duration_ms(2500)), "2.50s-");
        assert_eq!(format!("{:-^7}", display_duration_ms(2500)), "-2.50s-");
        assert_eq!(format!("{:-^8}", display_duration_ms(2500)), "-2.50s--");
    }
}
