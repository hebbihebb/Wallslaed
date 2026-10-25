// Link libgstreamer directly; only the runtime library is required, not the
// development package.
fn main() {
    println!("cargo:rustc-link-arg=-l:libgstreamer-1.0.so.0");
}
