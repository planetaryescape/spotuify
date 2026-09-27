fn main() {
    // The Sonic speed engine only exists for the embedded player; the
    // mock/remote-only builds carry no C at all.
    #[cfg(feature = "embedded-playback")]
    {
        println!("cargo:rerun-if-changed=vendor/sonic/sonic.c");
        println!("cargo:rerun-if-changed=vendor/sonic/sonic.h");
        cc::Build::new()
            .file("vendor/sonic/sonic.c")
            .include("vendor/sonic")
            .warnings(false)
            .compile("spotuify_sonic");
    }
}
