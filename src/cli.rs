// SPDX-License-Identifier: AGPL-3.0-or-later

use clap::{Arg, builder::TypedValueParser};

pub fn retry_on_timeout() -> Arg {
    Arg::new("retry_on_timeout")
        .long("retry-on-timeout")
        .value_name("N")
        .help("Retry timed-out input reads up to N times per file (0 disables retries, -1 retries indefinitely)")
        .allow_negative_numbers(true)
        .value_parser(clap::value_parser!(i64).range(-1..).map(|n| u64::try_from(n).ok()))
        .default_value("0")
}
