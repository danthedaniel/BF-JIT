const JIT_TARGETS: [&str; 6] = [
    "aarch64-unknown-linux-gnu",
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
];

fn main() {
    let target = std::env::var("TARGET").expect("TARGET env var not present");
    let jit_supported = JIT_TARGETS.contains(&target.as_str());

    if jit_supported {
        println!("cargo:rustc-cfg=feature=\"jit\"");
    }
}
