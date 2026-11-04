//! Console interface sharing the GUI's engine:
//!   dusty scan                 sizes of every item
//!   dusty clean quick|deep|ID… clean (admin items need an elevated console)
//!   dusty sensors              live readings, five samples

use std::time::Duration;

use crate::clean::{self, ITEMS, Item, Tier};
use crate::monitor::Sampler;
use crate::win::{fmt_bytes, is_elevated};

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("scan") => report(&ITEMS.iter().collect::<Vec<_>>(), false),
        Some("clean") => match select(&args[1..]) {
            Some(items) if !items.is_empty() => report(&items, true),
            _ => usage(),
        },
        Some("sensors") => sensors(),
        _ => usage(),
    }
}

fn usage() -> i32 {
    println!("usage: dusty [scan | clean quick|deep|<id>... | sensors]");
    println!("ids: {}", ITEMS.iter().map(|i| i.id).collect::<Vec<_>>().join(", "));
    2
}

fn select(args: &[String]) -> Option<Vec<&'static Item>> {
    let mut out = Vec::new();
    for a in args {
        match a.as_str() {
            "quick" => out.extend(ITEMS.iter().filter(|i| i.tier == Tier::Quick && i.default_on)),
            "deep" => out.extend(ITEMS.iter().filter(|i| i.default_on)),
            id => out.push(clean::catalog::find(id)?),
        }
    }
    Some(out)
}

fn report(items: &[&'static Item], delete: bool) -> i32 {
    let elevated = is_elevated();
    let mut total = 0;
    let mut results = Vec::new();
    for item in items {
        if item.admin && !elevated {
            println!("{:<18} {:>10}  needs an elevated console", item.id, "-");
            continue;
        }
        let o = clean::run(item, delete);
        total += o.tally.bytes;
        let size = if o.failed { "-".to_string() } else { fmt_bytes(o.tally.bytes) };
        println!("{:<18} {:>10}  {}", item.id, size, o.note.as_deref().unwrap_or(""));
        results.push((*item, o));
    }
    println!("{:<18} {:>10}", if delete { "freed" } else { "total" }, fmt_bytes(total));
    if delete {
        clean::log(&results);
    }
    0
}

fn sensors() -> i32 {
    let mut s = Sampler::new();
    for _ in 0..5 {
        std::thread::sleep(Duration::from_secs(1));
        let n = s.sample();
        let t = |v: Option<f32>| v.map_or("-".to_string(), |v| format!("{v:.0}°C"));
        let gpu = n.gpu.as_ref().map_or("-".to_string(), |g| {
            format!("{:.0}% {} {}/{}", g.util, t(g.temp), fmt_bytes(g.vram_used), fmt_bytes(g.vram_total))
        });
        println!(
            "cpu {:>3.0}% {:<5} mem {:>3.0}% disk {:>3.0}% {:<5} r {}/s w {}/s  gpu {gpu}",
            n.cpu,
            t(n.cpu_temp),
            n.mem_percent(),
            n.disk_active,
            t(n.disk_temp),
            fmt_bytes(n.disk_read as u64),
            fmt_bytes(n.disk_write as u64),
        );
    }
    0
}
