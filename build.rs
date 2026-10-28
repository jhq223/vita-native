fn main() {
    println!("cargo:rerun-if-changed=src/allocator/native.c");
    println!("cargo:rerun-if-changed=src/allocator/mock_sdk.h");
    println!("cargo:rerun-if-changed=src/allocator/mock_sdk.c");
    if std::env::var_os("CARGO_FEATURE_PROCESS_ALLOCATOR").is_none() {
        return;
    }
    let mut build = cc::Build::new();
    build.flag_if_supported("/experimental:c11atomics");
    build.file("src/allocator/native.c").std("c11");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("vita") {
        build.flag("-fno-builtin");
    } else {
        build.define("VITA_HEAP_TEST", None);
        build.define("_GNU_SOURCE", None);
        build.file("src/allocator/mock_sdk.c");
    }
    build.compile("vita_process_heap");
}
