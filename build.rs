#[rustfmt::skip]
fn main() {
  let target = std::env::var("TARGET").unwrap();

  let jit_supported =
      target == "aarch64-unknown-linux-gnu" ||
      target == "x86_64-unknown-linux-gnu" ||
      target == "aarch64-apple-darwin" ||
      target == "x86_64-apple-darwin";

  if jit_supported {
    println!("cargo:rustc-cfg=feature=\"jit\"");
  }
}
