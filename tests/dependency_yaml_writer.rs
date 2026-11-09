// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors

use serde::Serialize;
use std::io::{self, Write};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct DropSpy {
    drops: Arc<AtomicUsize>,
    fail_write: bool,
}
impl Drop for DropSpy {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}
impl Write for DropSpy {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.fail_write {
            Err(io::Error::other("intentional fixture write error"))
        } else {
            Ok(bytes.len())
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn writer(fail_write: bool) -> (DropSpy, Arc<AtomicUsize>) {
    let drops = Arc::new(AtomicUsize::new(0));
    (
        DropSpy {
            drops: Arc::clone(&drops),
            fail_write,
        },
        drops,
    )
}
fn check(name: &str, drops: &AtomicUsize, expected: usize) {
    let actual = drops.load(Ordering::SeqCst);
    println!("{name}: drops={actual}, expected={expected}");
    assert_eq!(actual, expected, "{name}");
}
struct FailSerialize;
impl Serialize for FailSerialize {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom(
            "intentional fixture serialization error",
        ))
    }
}
#[test]
fn to_writer_success_drops_owned_writer_once() {
    let (w, n) = writer(false);
    serde_yaml_ng::to_writer(w, &"text").unwrap();
    check("to_writer_success", &n, 1);
}
#[test]
fn to_writer_write_error_drops_owned_writer_once() {
    let (w, n) = writer(true);
    assert!(serde_yaml_ng::to_writer(w, &"x".repeat(32768)).is_err());
    check("to_writer_write_error", &n, 1);
}
#[test]
fn to_writer_serialization_error_drops_owned_writer_once() {
    let (w, n) = writer(false);
    assert!(serde_yaml_ng::to_writer(w, &FailSerialize).is_err());
    check("to_writer_serialization_error", &n, 1);
}
#[test]
fn ordinary_serializer_drop_releases_owned_writer_once() {
    let (w, n) = writer(false);
    let s = serde_yaml_ng::Serializer::new(w);
    check("serializer_before_drop", &n, 0);
    drop(s);
    check("serializer_after_drop", &n, 1);
}
#[test]
fn explicit_into_inner_transfers_writer_then_drops_once() {
    let (w, n) = writer(false);
    let mut s = serde_yaml_ng::Serializer::new(w);
    "text".serialize(&mut s).unwrap();
    let w = s.into_inner().unwrap();
    check("into_inner_before_writer_drop", &n, 0);
    drop(w);
    check("into_inner_after_writer_drop", &n, 1);
}

struct PanicSerialize;
impl Serialize for PanicSerialize {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        panic!("intentional serialization unwind")
    }
}
#[test]
fn serialization_unwind_drops_owned_writer_once() {
    let (w, n) = writer(false);
    let result = std::panic::catch_unwind(|| serde_yaml_ng::to_writer(w, &PanicSerialize));
    assert!(result.is_err());
    check("serialization_unwind", &n, 1);
}
