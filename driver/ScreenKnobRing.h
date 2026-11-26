// The shared-memory ring between the ScreenKnob driver (writer) and the
// ScreenKnob app (reader). The app mirrors this layout in src/ring.rs.

#pragma once
#include <stdatomic.h>
#include <stdint.h>
#include <stddef.h>

#define SK_SHM_NAME      "/screenknob.audio.1"  // POSIX names are at most 31 characters
#define SK_RING_MAGIC    0x534B4155u            // "SKAU"
#define SK_RING_VERSION  1u
#define SK_RING_FRAMES   65536u                 // about 1.4 s at 48 kHz

typedef struct {
    uint32_t magic;
    uint32_t version;
    uint32_t capacity;              // frames
    uint32_t channels;              // interleaved
    double sample_rate;
    _Atomic uint64_t write_end;     // absolute frame just after the last one written
    _Atomic uint32_t running;       // 1 while apps are playing to the device
    _Atomic uint32_t generation;    // goes up each time playing starts
    _Atomic uint32_t volume_bits;   // the device's volume (f32 bits, 0..1)
    _Atomic uint32_t muted;         // the device's mute
    uint8_t reserved[16];           // header is 64 bytes
    float data[];                   // capacity * channels samples
} SKRing;

_Static_assert(offsetof(SKRing, data) == 64, "ring header must be 64 bytes");

static inline size_t SKRingBytes(void) {
    return 64 + (size_t)SK_RING_FRAMES * 2 * sizeof(float);
}
