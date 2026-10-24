//! Temporary benchmark harness (removed after measuring).

use oilfind_core::{scan, Index};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args[0].as_str() {
        "scan" => {
            let roots: Vec<PathBuf> = args[2..].iter().map(PathBuf::from).collect();
            let cancel = AtomicBool::new(false);
            let started = Instant::now();
            let index = scan(&cancel, &roots, |_| {}).expect("scan");
            eprintln!("scan   {:>8.2?} entries={}", started.elapsed(), index.len());
            let started = Instant::now();
            index.save(Path::new(&args[1])).expect("save");
            eprintln!("save   {:>8.2?}", started.elapsed());
        }
        "load" => {
            let started = Instant::now();
            let loaded = Index::load(Path::new(&args[1])).expect("load");
            eprintln!("load   {:>8.2?} entries={}", started.elapsed(), loaded.len());
            let started = Instant::now();
            let (_, total) = loaded.search(&args[2], None, 3).expect("search");
            eprintln!("search {:>8.2?} total={}", started.elapsed(), total);
        }
        _ => panic!("scan|load"),
    }
}
