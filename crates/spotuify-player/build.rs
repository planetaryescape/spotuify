fn main() {
    // The Signalsmith Stretch shim only exists for the embedded player; the
    // mock/remote-only builds carry no C++ at all.
    #[cfg(feature = "embedded-playback")]
    {
        println!("cargo:rerun-if-changed=vendor/signalsmith-stretch/shim.cpp");
        println!("cargo:rerun-if-changed=vendor/signalsmith-stretch/signalsmith-stretch.h");
        println!("cargo:rerun-if-changed=vendor/signalsmith-stretch/dsp");
        let mut build = cc::Build::new();
        build
            .cpp(true)
            .file("vendor/signalsmith-stretch/shim.cpp")
            .include("vendor/signalsmith-stretch")
            .warnings(false);
        if build.get_compiler().is_like_msvc() {
            build.flag("/std:c++14").flag("/EHsc");
        } else {
            build.flag("-std=c++14");
        }
        build.compile("spotuify_signalsmith_stretch");
    }
}
