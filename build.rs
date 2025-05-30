const JIT_TARGETS: [&str; 4] = [
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
];

fn main() {
    let target = std::env::var("TARGET").unwrap();
    let jit_supported = JIT_TARGETS.contains(&target.as_str());

    if jit_supported {
        println!("cargo:rustc-cfg=feature=\"jit\"");
    }
}
