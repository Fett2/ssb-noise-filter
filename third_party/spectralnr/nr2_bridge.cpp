// extern "C" bridge over AetherSDR::SpectralNR, for the Rust wrapper.
//
// MSVC's C++ name mangling differs from Rust's `extern "C++"`, so the
// class is wrapped in a small opaque-handle C API. All calls are thin
// forwards; the parameter setters use the class's std::atomic members,
// which makes them safe to call from the GUI thread while the audio
// thread is processing.

#include "SpectralNR.h"

#include <cstddef>

extern "C" {

void* nr2_create(int fftSize, int sampleRate, int overlap,
                 int useLegacyGainMethods)
{
    return static_cast<void*>(new AetherSDR::SpectralNR(
        fftSize, sampleRate, overlap,
        static_cast<bool>(useLegacyGainMethods)));
}

void nr2_destroy(void* handle)
{
    delete static_cast<AetherSDR::SpectralNR*>(handle);
}

void nr2_process(void* handle, const float* input, float* output,
                 int numSamples)
{
    static_cast<AetherSDR::SpectralNR*>(handle)->process(input, output,
                                                         numSamples);
}

void nr2_reset(void* handle)
{
    static_cast<AetherSDR::SpectralNR*>(handle)->reset();
}

void nr2_reset_transient(void* handle)
{
    static_cast<AetherSDR::SpectralNR*>(handle)->resetTransient();
}

void nr2_set_gain_max(void* handle, float v)
{
    static_cast<AetherSDR::SpectralNR*>(handle)->setGainMax(v);
}

void nr2_set_gain_floor(void* handle, float v)
{
    static_cast<AetherSDR::SpectralNR*>(handle)->setGainFloor(v);
}

void nr2_set_qspp(void* handle, float v)
{
    static_cast<AetherSDR::SpectralNR*>(handle)->setQspp(v);
}

void nr2_set_gain_smooth(void* handle, float v)
{
    static_cast<AetherSDR::SpectralNR*>(handle)->setGainSmooth(v);
}

void nr2_set_gain_method(void* handle, int m)
{
    static_cast<AetherSDR::SpectralNR*>(handle)->setGainMethod(m);
}

void nr2_set_npe_method(void* handle, int m)
{
    static_cast<AetherSDR::SpectralNR*>(handle)->setNpeMethod(m);
}

void nr2_set_ae_filter(void* handle, int on)
{
    static_cast<AetherSDR::SpectralNR*>(handle)->setAeFilter(on != 0);
}

void nr2_set_post2_run(void* handle, int on)
{
    static_cast<AetherSDR::SpectralNR*>(handle)->setPost2Run(on != 0);
}

void nr2_set_post2_factor(void* handle, float v)
{
    static_cast<AetherSDR::SpectralNR*>(handle)->setPost2Factor(v);
}

void nr2_set_post2_nlevel(void* handle, float v)
{
    static_cast<AetherSDR::SpectralNR*>(handle)->setPost2Nlevel(v);
}

void nr2_set_post2_taper_hz(void* handle, float hz)
{
    static_cast<AetherSDR::SpectralNR*>(handle)->setPost2TaperHz(hz);
}

void nr2_set_post2_decay_seconds(void* handle, float seconds)
{
    static_cast<AetherSDR::SpectralNR*>(handle)
        ->setPost2DecaySeconds(seconds);
}

// Diagnostics / table verification.

int nr2_gamma_table_valid(void)
{
    return AetherSDR::SpectralNR::gammaTablesValid() ? 1 : 0;
}

double nr2_gamma_table_value(int index)
{
    return AetherSDR::SpectralNR::gammaTableValue(index);
}

unsigned long long nr2_transient_reset_count(const void* handle)
{
    return static_cast<const AetherSDR::SpectralNR*>(handle)
        ->transientResetCount();
}

unsigned long long nr2_noise_estimate_reset_count(const void* handle)
{
    return static_cast<const AetherSDR::SpectralNR*>(handle)
        ->noiseEstimateResetCount();
}

}  // extern "C"
