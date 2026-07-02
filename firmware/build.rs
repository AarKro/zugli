fn main() {
    linker_be_nice();
    // make sure linkall.x is the last linker script (otherwise might cause problems with flip-link)
    println!("cargo:rustc-link-arg=-Tlinkall.x");
    gzip_config_page();
}

/// Pre-compress the config page so `httpd::index` can serve it with `Content-Encoding: gzip`.
///
/// The uncompressed page is ~118 KB; blasting all of it through the WiFi stack on a reload
/// momentarily drained the driver's static TX buffer pool, surfacing as `esp_wifi_internal_tx
/// returned error: 257` (ESP_ERR_NO_MEM) backpressure while smoltcp retransmitted. Gzip shrinks
/// it ~5-7×, keeping the burst under that pressure point. The `.gz` lands in `OUT_DIR` and is
/// `include_bytes!`'d into flash `.rodata`, so it costs no RAM (and less flash than the plaintext).
fn gzip_config_page() {
    use std::io::Write as _;

    // build.rs runs with CWD = the package dir (firmware/); the page lives at the repo root.
    const SRC: &str = "../web/index.html";
    println!("cargo:rerun-if-changed={SRC}");

    let html = std::fs::read(SRC).expect("build.rs: read ../web/index.html");
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    enc.write_all(&html).expect("build.rs: gzip write");
    let gz = enc.finish().expect("build.rs: gzip finish");

    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("index.html.gz");
    std::fs::write(&out, &gz).expect("build.rs: write index.html.gz");
}

fn linker_be_nice() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let kind = &args[1];
        let what = &args[2];

        match kind.as_str() {
            "undefined-symbol" => match what.as_str() {
                what if what.starts_with("_defmt_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `defmt` not found - make sure `defmt.x` is added as a linker script and you have included `use defmt_rtt as _;`"
                    );
                    eprintln!();
                }
                "_stack_start" => {
                    eprintln!();
                    eprintln!("💡 Is the linker script `linkall.x` missing?");
                    eprintln!();
                }
                what if what.starts_with("esp_rtos_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `esp-radio` has no scheduler enabled. Make sure you have initialized `esp-rtos` or provided an external scheduler."
                    );
                    eprintln!();
                }
                "embedded_test_linker_file_not_added_to_rustflags" => {
                    eprintln!();
                    eprintln!(
                        "💡 `embedded-test` not found - make sure `embedded-test.x` is added as a linker script for tests"
                    );
                    eprintln!();
                }
                "free"
                | "malloc"
                | "calloc"
                | "get_free_internal_heap_size"
                | "malloc_internal"
                | "realloc_internal"
                | "calloc_internal"
                | "free_internal" => {
                    eprintln!();
                    eprintln!(
                        "💡 Did you forget the `esp-alloc` dependency or didn't enable the `compat` feature on it?"
                    );
                    eprintln!();
                }
                _ => (),
            },
            // we don't have anything helpful for "missing-lib" yet
            _ => {
                std::process::exit(1);
            }
        }

        std::process::exit(0);
    }

    println!(
        "cargo:rustc-link-arg=-Wl,--error-handling-script={}",
        std::env::current_exe().unwrap().display()
    );
}
