// C ABI over the header-only Signalsmith Stretch (MIT, vendored alongside).
//
// A plain `extern "C"` surface keeps the Rust side to a handful of FFI
// declarations: no cxx/bindgen, and no dependence on a third-party binding
// crate's C++ polyfills (the `ssstretch` crate's `std::make_unique` shim is
// ambiguous under MSVC, which reports `__cplusplus` as 199711L by default).

#include "signalsmith-stretch.h"

#include <cstdint>

using Stretch = signalsmith::stretch::SignalsmithStretch<float>;

extern "C" {

Stretch *spotuify_stretch_new(int32_t channels, float sample_rate) {
    Stretch *stretch = new (std::nothrow) Stretch();
    if (stretch != nullptr) {
        stretch->presetDefault(channels, sample_rate);
    }
    return stretch;
}

void spotuify_stretch_free(Stretch *stretch) { delete stretch; }

void spotuify_stretch_reset(Stretch *stretch) { stretch->reset(); }

int32_t spotuify_stretch_input_latency(const Stretch *stretch) { return stretch->inputLatency(); }

int32_t spotuify_stretch_output_latency(const Stretch *stretch) { return stretch->outputLatency(); }

// `inputs` / `outputs` are arrays of `channels` planar channel pointers.
void spotuify_stretch_process(Stretch *stretch, const float *const *inputs, int32_t input_samples,
                              float *const *outputs, int32_t output_samples) {
    stretch->process(inputs, input_samples, outputs, output_samples);
}

}  // extern "C"
