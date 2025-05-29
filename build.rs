#[rustfmt::skip]
fn main() {
  let jit_supported =
      cfg!(any(target_os = "linux", target_arch = "aarch64")) ||
      cfg!(any(target_os = "linux", target_arch = "x86_64")) ||
      cfg!(any(target_os = "macos", target_arch = "aarch64")) ||
      cfg!(any(target_os = "macos", target_arch = "x86_64"));

  if jit_supported {
    println!("cargo:rustc-cfg=feature=\"jit\"");
  }
}
