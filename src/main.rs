use std::env;
use std::process::{self, Command};
use std::time::{SystemTime, UNIX_EPOCH};

fn print_usage(program_name: &str) {
    println!("Usage:");
    println!("  {program_name} [options]");
    println!();
    println!("Options:");
    println!("  -p, --pretty        show downtime in pretty format");
    println!("  -h, --help          display this help and exit");
    println!("  -s, --since <when>  compute downtime since the given time");
    println!("                      (e.g. \"2026-04-01\", \"yesterday\", \"2 weeks ago\")");
    println!("  -V, --version       output version information and exit");
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

fn parse_when(s: &str) -> Option<u64> {
    let try_gnu = |cmd: &str| -> Option<u64> {
        let out = Command::new(cmd).args(["-d", s, "+%s"]).output().ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    };
    if let Some(t) = try_gnu("date") {
        return Some(t);
    }
    if let Some(t) = try_gnu("gdate") {
        return Some(t);
    }
    for fmt in [
        "%Y-%m-%d",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
    ] {
        let out = Command::new("date")
            .args(["-j", "-f", fmt, s, "+%s"])
            .output()
            .ok();
        if let Some(out) = out {
            if out.status.success() {
                if let Ok(t) = String::from_utf8_lossy(&out.stdout).trim().parse() {
                    return Some(t);
                }
            }
        }
    }
    None
}

// ---- linux via journalctl ----

struct Boot {
    boot_id: String,
    first_entry_us: u64,
    last_entry_us: u64,
}

fn list_boots() -> Result<Vec<Boot>, String> {
    let out = Command::new("journalctl")
        .args(["--list-boots", "-o", "json"])
        .output()
        .map_err(|e| format!("couldn't run journalctl ({e})"))?;
    if !out.status.success() {
        return Err(format!(
            "journalctl exited non-zero: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut boots =
        parse_boots_json(&s).ok_or_else(|| "couldn't parse journalctl JSON".to_string())?;
    boots.sort_by_key(|b| b.first_entry_us);
    Ok(boots)
}

fn parse_boots_json(s: &str) -> Option<Vec<Boot>> {
    let mut boots = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            let rel_end = s[i..].find('}')?;
            let obj = &s[i..i + rel_end + 1];
            let boot_id = extract_str_field(obj, "boot_id")?;
            let first_entry_us = extract_num_field(obj, "first_entry")?;
            let last_entry_us = extract_num_field(obj, "last_entry")?;
            boots.push(Boot {
                boot_id,
                first_entry_us,
                last_entry_us,
            });
            i += rel_end + 1;
        } else {
            i += 1;
        }
    }
    Some(boots)
}

fn extract_str_field(obj: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = obj.find(&needle)? + needle.len();
    let rest = &obj[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_num_field(obj: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\":");
    let start = obj.find(&needle)? + needle.len();
    let rest = &obj[start..];
    let end = rest.find(|c: char| c == ',' || c == '}')?;
    rest[..end].trim().parse().ok()
}

fn userspace_ready_us(boot_id: &str) -> Option<u64> {
    let out = Command::new("journalctl")
        .args([
            "_PID=1",
            "-b",
            boot_id,
            "-g",
            "Startup finished in.*=",
            "-o",
            "short-unix",
            "-n",
            "1",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let first = line.split_whitespace().next()?;
    let (secs, frac) = first.split_once('.')?;
    let secs: u64 = secs.parse().ok()?;
    let frac_us: u64 = frac.get(..6)?.parse().ok()?;
    Some(secs * 1_000_000 + frac_us)
}

fn linux_windows() -> Result<(Vec<(u64, u64)>, u64), String> {
    let boots = list_boots()?;
    if boots.is_empty() {
        return Err("journalctl returned no boots".into());
    }
    let oldest = boots[0].first_entry_us;
    let mut wins = Vec::new();
    for w in boots.windows(2) {
        let s = w[0].last_entry_us;
        let e = userspace_ready_us(&w[1].boot_id).unwrap_or(w[1].first_entry_us);
        if e > s {
            wins.push((s, e));
        }
    }
    Ok((wins, oldest))
}

// ---- macos via pmset + last ----

fn macos_windows() -> Result<(Vec<(u64, u64)>, u64), String> {
    let mut all: Vec<(u64, u64)> = Vec::new();
    let mut oldest = u64::MAX;
    let mut errs: Vec<String> = Vec::new();

    match macos_pmset_windows() {
        Ok((wins, o)) => {
            all.extend(wins);
            oldest = oldest.min(o);
        }
        Err(e) => errs.push(format!("pmset: {e}")),
    }

    match macos_last_windows() {
        Ok((wins, o)) => {
            all.extend(wins);
            oldest = oldest.min(o);
        }
        Err(e) => errs.push(format!("last: {e}")),
    }

    if oldest == u64::MAX {
        return Err(errs.join("; "));
    }
    Ok((coalesce(all), oldest))
}

fn macos_pmset_windows() -> Result<(Vec<(u64, u64)>, u64), String> {
    let out = Command::new("pmset")
        .args(["-g", "log"])
        .output()
        .map_err(|e| format!("couldn't run pmset ({e})"))?;
    if !out.status.success() {
        return Err(format!(
            "pmset exited non-zero: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let log = String::from_utf8_lossy(&out.stdout);
    parse_pmset_log(&log)
}

fn macos_last_windows() -> Result<(Vec<(u64, u64)>, u64), String> {
    let shutdowns = run_last("shutdown")?;
    let reboots = run_last("reboot")?;

    let oldest = shutdowns
        .iter()
        .chain(reboots.iter())
        .copied()
        .min()
        .ok_or_else(|| "no reboot/shutdown entries".to_string())?;

    Ok((pair_shutdowns_with_reboots(&shutdowns, &reboots), oldest))
}

fn pair_shutdowns_with_reboots(shutdowns: &[u64], reboots: &[u64]) -> Vec<(u64, u64)> {
    // Both inputs sorted ascending. For each shutdown S, find the smallest reboot R > S; (S, R) is the powered-off window.
    let mut wins = Vec::new();
    let mut ri = 0usize;
    for &s in shutdowns {
        while ri < reboots.len() && reboots[ri] <= s {
            ri += 1;
        }
        if ri < reboots.len() {
            wins.push((s, reboots[ri]));
        }
    }
    wins
}

fn run_last(kind: &str) -> Result<Vec<u64>, String> {
    let out = Command::new("last")
        .args(["-F", kind])
        .output()
        .map_err(|e| format!("couldn't run last ({e})"))?;
    if !out.status.success() {
        return Err(format!(
            "last exited non-zero: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut events = parse_last_output(&text);
    events.sort();
    Ok(events)
}

fn parse_last_output(text: &str) -> Vec<u64> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("wtmp begins") || trimmed.starts_with("btmp begins") {
            continue;
        }
        if let Some(t) = extract_last_date(line) {
            out.push(t);
        }
    }
    out
}

fn extract_last_date(line: &str) -> Option<u64> {
    // Find the first occurrence of " <DOW> " where DOW is a 3-letter weekday.
    // Date format from `last -F` is "Day Mon DD HH:MM:SS YYYY", possibly with a double space between Mon and a single-digit day (BSD `last`).
    let dows = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let mut start = None;
    for dow in dows {
        let needle = format!(" {dow} ");
        if let Some(idx) = line.find(&needle) {
            start = Some(idx + 1);
            break;
        }
    }
    let start = start?;
    let after = &line[start..];
    // Take up to 25 chars: "Day Mon DD HH:MM:SS YYYY" is 24, BSD double-space form ("Day Mon  D HH:MM:SS YYYY") is also 24.
    let max = after.len().min(26);
    let date_str = after[..max].trim();

    // GNU date first (handles flexible inputs).
    if let Ok(out) = Command::new("date").args(["-d", date_str, "+%s"]).output() {
        if out.status.success() {
            if let Ok(t) = String::from_utf8_lossy(&out.stdout).trim().parse::<u64>() {
                return Some(t * 1_000_000);
            }
        }
    }
    for fmt in ["%a %b %e %H:%M:%S %Y", "%a %b %d %H:%M:%S %Y"] {
        if let Ok(out) = Command::new("date")
            .args(["-j", "-f", fmt, date_str, "+%s"])
            .output()
        {
            if out.status.success() {
                if let Ok(t) = String::from_utf8_lossy(&out.stdout).trim().parse::<u64>() {
                    return Some(t * 1_000_000);
                }
            }
        }
    }
    None
}

fn coalesce(mut wins: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    wins.sort_by_key(|w| w.0);
    let mut out: Vec<(u64, u64)> = Vec::new();
    for w in wins {
        if let Some(last) = out.last_mut() {
            if w.0 <= last.1 {
                last.1 = last.1.max(w.1);
                continue;
            }
        }
        out.push(w);
    }
    out
}

fn parse_pmset_log(log: &str) -> Result<(Vec<(u64, u64)>, u64), String> {
    let mut wins: Vec<(u64, u64)> = Vec::new();
    let mut sleep_start: Option<u64> = None;
    let mut oldest: Option<u64> = None;

    for line in log.lines() {
        // Lines we care about start with "YYYY-MM-DD HH:MM:SS ±ZZZZ".
        if line.len() < 25 {
            continue;
        }
        let Some(ts) = parse_pmset_ts(&line[..25]) else {
            continue;
        };
        if oldest.is_none() {
            oldest = Some(ts);
        }
        let rest = line[25..].trim_start();
        let domain = rest.split_whitespace().next().unwrap_or("");
        match domain {
            // entering an unavailable state
            "Sleep" => {
                if sleep_start.is_none() {
                    sleep_start = Some(ts);
                }
            }
            "Wake" => {
                if let Some(s) = sleep_start.take() {
                    if ts > s {
                        wins.push((s, ts));
                    }
                }
            }
            _ => {}
        }
    }

    let oldest = oldest.ok_or_else(|| "pmset log had no parseable entries".to_string())?;
    Ok((wins, oldest))
}

// "2026-04-25 14:30:21 -0700" -> microseconds since epoch.
fn parse_pmset_ts(s: &str) -> Option<u64> {
    let bytes = s.as_bytes();
    if bytes.len() != 25 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b' ' {
        return None;
    }
    if bytes[13] != b':' || bytes[16] != b':' || bytes[19] != b' ' {
        return None;
    }
    let y: i64 = s[0..4].parse().ok()?;
    let mo: i64 = s[5..7].parse().ok()?;
    let d: i64 = s[8..10].parse().ok()?;
    let h: i64 = s[11..13].parse().ok()?;
    let mi: i64 = s[14..16].parse().ok()?;
    let se: i64 = s[17..19].parse().ok()?;
    let sign: i64 = match bytes[20] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let tz_h: i64 = s[21..23].parse().ok()?;
    let tz_m: i64 = s[23..25].parse().ok()?;

    // days from epoch
    let y_adj = y - if mo <= 2 { 1 } else { 0 };
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = y_adj - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;

    let local_secs = days * 86400 + h * 3600 + mi * 60 + se;
    let tz_secs = sign * (tz_h * 3600 + tz_m * 60);
    let utc_secs = local_secs - tz_secs;
    if utc_secs < 0 {
        return None;
    }
    Some((utc_secs as u64) * 1_000_000)
}

fn get_windows() -> Result<(Vec<(u64, u64)>, u64), String> {
    match linux_windows() {
        Ok(x) => Ok(x),
        Err(linux_err) => match macos_windows() {
            Ok(x) => Ok(x),
            Err(macos_err) => Err(format!(
                "no working backend.\n  journalctl: {linux_err}\n  pmset: {macos_err}"
            )),
        },
    }
}

fn fmt_pretty(secs: u64) -> String {
    let d = secs / 86400;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    let mut parts = Vec::new();
    if d > 0 {
        parts.push(format!("{d}d"));
    }
    if h > 0 {
        parts.push(format!("{h}h"));
    }
    if m > 0 {
        parts.push(format!("{m}m"));
    }
    if s > 0 || parts.is_empty() {
        parts.push(format!("{s}s"));
    }
    parts.join(" ")
}

fn fmt_unix_secs(secs: u64) -> String {
    Command::new("date")
        .args(["-d", &format!("@{secs}"), "+%Y-%m-%d %H:%M:%S %Z"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| {
            // BSD date fallback
            Command::new("date")
                .args(["-r", &secs.to_string(), "+%Y-%m-%d %H:%M:%S %Z"])
                .output()
                .ok()
                .and_then(|o| {
                    if o.status.success() {
                        Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| format!("unix:{secs}"))
        })
}

fn no_backend_bail(reason: &str) -> ! {
    eprintln!("downtime: can't determine downtime windows.");
    eprintln!("{reason}");
    eprintln!("downtime: needs journalctl (linux+systemd) or pmset (macOS). neither was happy.");
    process::exit(1);
}

fn compute_downtime_since(since_secs: u64, pretty: bool) {
    let (windows, oldest_known) = match get_windows() {
        Ok(x) => x,
        Err(e) => no_backend_bail(&e),
    };

    let now = now_us();
    let since_us = since_secs.saturating_mul(1_000_000);

    if since_us > now {
        println!("you're asking about downtime in the future. it's not down then either.");
        println!("(probably. don't quote me on that.)");
        return;
    }

    let truncated = since_us < oldest_known;

    let mut total_us: u64 = 0;
    let mut clipped: Vec<(u64, u64)> = Vec::new();
    for &(start, end) in &windows {
        let s = start.max(since_us);
        let e = end.min(now);
        if e > s {
            total_us += e - s;
            clipped.push((s, e));
        }
    }

    let total_secs = total_us / 1_000_000;

    if pretty {
        // If the request predates what we have, measure against [oldest_known, now] so the percentage reflects what we could actually see,
        // not invisible time we'd be claiming as up.
        let window_start = if truncated { oldest_known } else { since_us };
        let window_us = now.saturating_sub(window_start);
        let pct = if window_us > 0 {
            (total_us as f64) * 100.0 / (window_us as f64)
        } else {
            0.0
        };

        if clipped.is_empty() {
            println!("no recorded downtime since the requested time. show-off.");
        } else {
            let qualifier = if pct <= 0.5 {
                "more reliable than github".to_string()
            } else {
                format!("over {pct:.1}%")
            };
            println!("downtime: {} ({qualifier})", fmt_pretty(total_secs));
            println!(
                "across {} window{}.",
                clipped.len(),
                if clipped.len() == 1 { "" } else { "s" }
            );
        }
        if truncated {
            println!(
                "(only known back to {}; older downtime is invisible to us.)",
                fmt_unix_secs(oldest_known / 1_000_000)
            );
        }
    } else {
        println!("{total_secs}");
    }
}

fn take_value(i: &mut usize, args: &[String], flag: &str) -> String {
    *i += 1;
    if *i >= args.len() {
        eprintln!("option `{flag}` requires a value");
        process::exit(1);
    }
    args[*i].clone()
}

fn main() {
    let mut help = false;
    let mut pretty = false;
    let mut since: Option<String> = None;
    let mut version = false;

    let args: Vec<String> = env::args().collect();
    let program_name = args[0].clone();
    let mut i = 1;
    while i < args.len() {
        let arg = args[i].clone();
        if let Some(rest) = arg.strip_prefix("--") {
            if let Some((k, v)) = rest.split_once('=') {
                match k {
                    "since" => since = Some(v.to_string()),
                    _ => {
                        println!("Unrecognized option `--{k}`");
                        print_usage(&program_name);
                        process::exit(1);
                    }
                }
            } else {
                match rest {
                    "help" => help = true,
                    "pretty" => pretty = true,
                    "since" => since = Some(take_value(&mut i, &args, "--since")),
                    "version" => version = true,
                    _ => {
                        println!("Unrecognized option `--{rest}`");
                        print_usage(&program_name);
                        process::exit(1);
                    }
                }
            }
        } else if let Some(rest) = arg.strip_prefix("-") {
            let chars: Vec<char> = rest.chars().collect();
            let mut j = 0;
            while j < chars.len() {
                match chars[j] {
                    'h' => help = true,
                    'p' => pretty = true,
                    's' => {
                        if j + 1 < chars.len() {
                            since = Some(chars[j + 1..].iter().collect());
                            j = chars.len();
                        } else {
                            since = Some(take_value(&mut i, &args, "-s"));
                        }
                    }
                    'V' => version = true,
                    c => {
                        println!("Unrecognized option `-{c}`");
                        print_usage(&program_name);
                        process::exit(1);
                    }
                }
                j += 1;
            }
        } else {
            println!("Unrecognized argument `{arg}`");
            print_usage(&program_name);
            process::exit(1);
        }
        i += 1;
    }

    if help {
        print_usage(&program_name);
        return;
    }
    if version {
        println!("downtime version 1.0");
        return;
    }

    if let Some(s) = since {
        let Some(ts) = parse_when(&s) else {
            eprintln!(
                "downtime: couldn't parse `{s}` as a time. try `2026-04-01`, `yesterday`, `2 weeks ago`."
            );
            process::exit(1);
        };
        compute_downtime_since(ts, pretty);
        return;
    }

    println!("System is not down.");
}
