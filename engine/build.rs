use std::{env, path::PathBuf};
fn main() {
    println!("cargo:rerun-if-changed=native/decoder.cpp");
    println!("cargo:rerun-if-changed=.native/lib/libdsdcc.1.dylib");
    for key in ["DSDCC_INCLUDE_DIR", "DSDCC_LIB_DIR"] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let include = env::var_os("DSDCC_INCLUDE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if root.join(".native/src/dsdcc/dsd_decoder.h").is_file() {
                root.join(".native/src/dsdcc")
            } else {
                PathBuf::from("/tmp/cory-radio-dsdcc-20261019")
            }
        });
    let lib = env::var_os("DSDCC_LIB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if root.join(".native/lib").is_dir() {
                root.join(".native/lib")
            } else {
                PathBuf::from(env::var("HOME").unwrap()).join("projects/amateur-radio/.tools/lib")
            }
        });
    assert!(
        include.join("dsd_decoder.h").is_file(),
        "DSDcc headers missing; set DSDCC_INCLUDE_DIR (see README)"
    );
    cc::Build::new()
        .cpp(true)
        .std("c++11")
        .include(include)
        .file("native/decoder.cpp")
        .compile("radio_codec");
    let link_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let link = link_dir.join(if cfg!(target_os = "macos") {
        "libdsdcc.dylib"
    } else {
        "libdsdcc.so"
    });
    let actual = lib.join(if cfg!(target_os = "macos") {
        "libdsdcc.1.dylib"
    } else {
        "libdsdcc.so"
    });
    if link.is_symlink() {
        std::fs::remove_file(&link).unwrap();
    }
    if !link.exists() {
        std::os::unix::fs::symlink(actual, &link).unwrap();
    }
    println!("cargo:rustc-link-search=native={}", link_dir.display());
    println!("cargo:rustc-link-lib=dylib=dsdcc");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
}
