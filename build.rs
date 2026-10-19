// Modified by flockr (https://github.com/Rambarani/flockr): changed from herdr v0.9.5 (Apache-2.0). See NOTICE.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=FLOCKR_BUILD_CHANNEL");
    println!("cargo:rerun-if-env-changed=FLOCKR_BUILD_ID");
    println!("cargo:rerun-if-env-changed=FLOCKR_BUILD_COMMIT");
}
