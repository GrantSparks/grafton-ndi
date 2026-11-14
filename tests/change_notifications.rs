//! Concurrent discovery lookups receive the same native invalidations.
#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, name);

use grafton_ndi::{Finder, FinderOptions, Sender, SenderOptions};
use std::{
    sync::{mpsc, Barrier},
    thread,
    time::Duration,
};

#[test]
fn concurrent_source_lookups_return_promptly_for_successive_announcements() {
    let Some(ndi) = common::ndi() else { return };
    let finder = Finder::new(&ndi, &FinderOptions::new().extra_ips("127.0.0.1")).unwrap();
    for round in 0..3 {
        let names = [
            common::unique_name(&format!("broadcast-{round}-A")),
            common::unique_name(&format!("broadcast-{round}-B")),
        ];
        let ready = Barrier::new(3);
        let (tx, rx) = mpsc::channel();
        thread::scope(|scope| {
            for (index, name) in names.iter().enumerate() {
                let (finder, ready, tx) = (&finder, &ready, tx.clone());
                scope.spawn(move || {
                    let suffix = format!("({name})");
                    ready.wait();
                    let source = finder
                        .find_source(Duration::from_secs(8), |source| {
                            source.name().ends_with(&suffix)
                        })
                        .unwrap();
                    tx.send((index, source)).unwrap();
                });
            }
            ready.wait();
            thread::sleep(Duration::from_millis(100));
            let first = Sender::new(&ndi, &SenderOptions::new(&names[0])).unwrap();
            let (index, source) = rx
                .recv_timeout(Duration::from_secs(2))
                .expect("a lookup missed the first source-list update");
            assert_eq!(index, 0);
            assert!(source.is_some());
            let second = Sender::new(&ndi, &SenderOptions::new(&names[1])).unwrap();
            let (index, source) = rx
                .recv_timeout(Duration::from_secs(2))
                .expect("a lookup missed the second source-list update");
            assert_eq!(index, 1);
            assert!(source.is_some());
            drop((first, second));
        });
    }
}
