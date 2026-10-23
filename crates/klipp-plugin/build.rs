//! The OFX parameter suite's variadic entry points are C (#2455): only C
//! reads a `va_list`.

fn main() {
    println!("cargo:rerun-if-changed=src/ofx_param_shim.c");
    cc::Build::new().file("src/ofx_param_shim.c").warnings(true).compile("klipp_ofx_param_shim");
}
