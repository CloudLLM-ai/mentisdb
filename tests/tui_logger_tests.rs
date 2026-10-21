//! The TUI logger channel is bounded and never blocks the logging thread.
#![cfg(feature = "server")]

use log::{Level, Log, Record};
use mentisdb::tui::{TuiLogger, LOG_CHANNEL_CAPACITY};
use std::sync::mpsc;

/// Flooding the logger with nobody draining drops the overflow instead of
/// blocking or buffering without bound.
#[test]
fn test_tui_logger_drops_when_channel_full() {
    let (tx, rx) = mpsc::sync_channel(LOG_CHANNEL_CAPACITY);
    let logger = TuiLogger::new(tx);
    for i in 0..(LOG_CHANNEL_CAPACITY * 2) {
        logger.log(
            &Record::builder()
                .level(Level::Info)
                .target("test")
                .args(format_args!("line {i}"))
                .build(),
        );
    }
    assert_eq!(rx.try_iter().count(), LOG_CHANNEL_CAPACITY);
}

/// Records above Info verbosity are filtered out.
#[test]
fn test_tui_logger_filters_debug() {
    let (tx, rx) = mpsc::sync_channel(8);
    let logger = TuiLogger::new(tx);
    logger.log(
        &Record::builder()
            .level(Level::Debug)
            .target("t")
            .args(format_args!("x"))
            .build(),
    );
    assert!(rx.try_recv().is_err());
}
