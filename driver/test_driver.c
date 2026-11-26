// Loads the ScreenKnob Audio driver in an ordinary process (no install needed),
// asks it what the sound server would, plays a tone into it and checks that the
// tone arrives in the shared-memory ring.
// Build both with scripts/make-driver.sh (they share sound under a test name, so
// they never touch the real driver), then:
//   target/test_driver target/driver-test.noindex/ScreenKnobAudio

#include <CoreAudio/AudioServerPlugIn.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <math.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>
#include <string.h>
#include <stdlib.h>
#include <mach/mach_time.h>
#include "ScreenKnobRing.h"

static int failures = 0;
#define CHECK(cond, ...) do { if (cond) printf("ok   "); else { printf("FAIL "); failures++; } printf(__VA_ARGS__); printf("\n"); } while (0)

static int notified = 0;
static OSStatus HostChanged(AudioServerPlugInHostRef h, AudioObjectID o, UInt32 n, const AudioObjectPropertyAddress *a) {
    (void)h; (void)o; (void)n; (void)a; notified++; return 0;
}
static OSStatus HostCopy(AudioServerPlugInHostRef h, CFStringRef k, CFPropertyListRef *o) { (void)h; (void)k; *o = NULL; return -1; }
static OSStatus HostWrite(AudioServerPlugInHostRef h, CFStringRef k, CFPropertyListRef v) { (void)h; (void)k; (void)v; return 0; }
static OSStatus HostDelete(AudioServerPlugInHostRef h, CFStringRef k) { (void)h; (void)k; return 0; }
static OSStatus HostConfig(AudioServerPlugInHostRef h, AudioObjectID d, UInt64 a, void *i) { (void)h; (void)d; (void)a; (void)i; return 0; }

int main(int argc, char **argv) {
    const char *path = argc > 1 ? argv[1] : "target/driver-test.noindex/ScreenKnobAudio";
    if (strcmp(SK_SHM_NAME, "/screenknob.audio.1") == 0) {
        printf("refusing to run against the real driver's shared sound; build with scripts/make-driver.sh\n");
        return 2;
    }
    void *lib = dlopen(path, RTLD_NOW);
    CHECK(lib != NULL, "load %s", path);
    if (!lib) return 1;
    void *(*create)(CFAllocatorRef, CFUUIDRef) = dlsym(lib, "ScreenKnobAudio_Create");
    CHECK(create != NULL, "factory found");
    AudioServerPlugInDriverRef drv = create(NULL, kAudioServerPlugInTypeUUID);
    CHECK(drv != NULL, "factory makes the driver");
    void *iface = NULL;
    CHECK((*drv)->QueryInterface(drv, CFUUIDGetUUIDBytes(kAudioServerPlugInDriverInterfaceUUID), &iface) == 0 && iface == drv, "driver interface");

    AudioServerPlugInHostInterface host = { HostChanged, HostCopy, HostWrite, HostDelete, HostConfig };
    AudioServerPlugInHostRef hostRef = &host;
    CHECK((*drv)->Initialize(drv, hostRef) == 0, "initialize");

    AudioObjectPropertyAddress a = { kAudioPlugInPropertyDeviceList, kAudioObjectPropertyScopeGlobal, 0 };
    AudioObjectID dev = 0; UInt32 sz = sizeof dev;
    CHECK((*drv)->GetPropertyData(drv, 1, 0, &a, 0, NULL, sz, &sz, &dev) == 0 && dev == 2, "plug-in lists device %u", dev);

    CFStringRef s = NULL;
    a.mSelector = kAudioObjectPropertyName; sz = sizeof s;
    (*drv)->GetPropertyData(drv, dev, 0, &a, 0, NULL, sz, &sz, &s);
    char buf[64] = ""; if (s) CFStringGetCString(s, buf, sizeof buf, kCFStringEncodingUTF8);
    CHECK(strcmp(buf, "ScreenKnob") == 0, "device name '%s'", buf);

    CFStringRef uid = CFSTR("ScreenKnob_Device"); AudioObjectID found = 0;
    a.mSelector = kAudioPlugInPropertyTranslateUIDToDevice; sz = sizeof found;
    (*drv)->GetPropertyData(drv, 1, 0, &a, sizeof uid, &uid, sz, &sz, &found);
    CHECK(found == 2, "UID finds the device");

    // Every property the device says it has must answer with the size it announced.
    AudioObjectPropertySelector sels[] = {
        kAudioObjectPropertyBaseClass, kAudioObjectPropertyClass, kAudioObjectPropertyOwner, kAudioObjectPropertyName,
        kAudioObjectPropertyManufacturer, kAudioObjectPropertyOwnedObjects, kAudioDevicePropertyDeviceUID,
        kAudioDevicePropertyModelUID, kAudioDevicePropertyTransportType, kAudioDevicePropertyRelatedDevices,
        kAudioDevicePropertyClockDomain, kAudioDevicePropertyDeviceIsAlive, kAudioDevicePropertyDeviceIsRunning,
        kAudioDevicePropertyDeviceCanBeDefaultDevice, kAudioDevicePropertyDeviceCanBeDefaultSystemDevice,
        kAudioDevicePropertyLatency, kAudioDevicePropertyStreams, kAudioObjectPropertyControlList,
        kAudioDevicePropertySafetyOffset, kAudioDevicePropertyNominalSampleRate,
        kAudioDevicePropertyAvailableNominalSampleRates, kAudioDevicePropertyIsHidden,
        kAudioDevicePropertyPreferredChannelsForStereo, kAudioDevicePropertyPreferredChannelLayout,
        kAudioDevicePropertyZeroTimeStampPeriod,
    };
    int bad = 0;
    for (size_t i = 0; i < sizeof sels / sizeof sels[0]; i++) {
        AudioObjectPropertyAddress p = { sels[i], kAudioObjectPropertyScopeOutput, 0 };
        UInt32 want = 0, got = 0; char data[512];
        if (!(*drv)->HasProperty(drv, dev, 0, &p)) { bad++; continue; }
        (*drv)->GetPropertyDataSize(drv, dev, 0, &p, 0, NULL, &want);
        if ((*drv)->GetPropertyData(drv, dev, 0, &p, 0, NULL, sizeof data, &got, data) != 0 || got != want) bad++;
    }
    CHECK(bad == 0, "all %zu device properties answer consistently", sizeof sels / sizeof sels[0]);

    AudioObjectPropertyAddress sf = { kAudioStreamPropertyVirtualFormat, kAudioObjectPropertyScopeGlobal, 0 };
    AudioStreamBasicDescription fmt; sz = sizeof fmt;
    (*drv)->GetPropertyData(drv, 3, 0, &sf, 0, NULL, sz, &sz, &fmt);
    CHECK(fmt.mSampleRate == 48000 && fmt.mChannelsPerFrame == 2 && fmt.mBitsPerChannel == 32, "stream format 48 kHz stereo float");

    // Volume: set, read back, dB view, change notification.
    AudioObjectPropertyAddress vol = { kAudioLevelControlPropertyScalarValue, kAudioObjectPropertyScopeGlobal, 0 };
    Boolean settable = false;
    (*drv)->IsPropertySettable(drv, 4, 0, &vol, &settable);
    CHECK(settable, "volume is settable");
    Float32 v = 0.25f; int before = notified;
    (*drv)->SetPropertyData(drv, 4, 0, &vol, 0, NULL, sizeof v, &v);
    Float32 back = 0; sz = sizeof back;
    (*drv)->GetPropertyData(drv, 4, 0, &vol, 0, NULL, sz, &sz, &back);
    AudioObjectPropertyAddress db = { kAudioLevelControlPropertyDecibelValue, kAudioObjectPropertyScopeGlobal, 0 };
    Float32 dbv = 0; sz = sizeof dbv;
    (*drv)->GetPropertyData(drv, 4, 0, &db, 0, NULL, sz, &sz, &dbv);
    CHECK(fabsf(back - 0.25f) < 1e-6 && notified > before, "volume 0.25 stored and announced (%.1f dB)", dbv);
    AudioObjectPropertyAddress mute = { kAudioBooleanControlPropertyValue, kAudioObjectPropertyScopeGlobal, 0 };
    UInt32 m = 1;
    (*drv)->SetPropertyData(drv, 5, 0, &mute, 0, NULL, sizeof m, &m);
    UInt32 mb = 0; sz = sizeof mb;
    (*drv)->GetPropertyData(drv, 5, 0, &mute, 0, NULL, sz, &sz, &mb);
    CHECK(mb == 1, "mute stored");
    m = 0; (*drv)->SetPropertyData(drv, 5, 0, &mute, 0, NULL, sizeof m, &m);

    // IO: start, time stamps, play a tone, read it from the shared memory.
    CHECK((*drv)->StartIO(drv, dev, 1) == 0, "start IO");
    Float64 st = -1; UInt64 ht = 0, seed = 0;
    CHECK((*drv)->GetZeroTimeStamp(drv, dev, 1, &st, &ht, &seed) == 0 && st == 0 && seed == 1, "zero time stamp");
    Boolean will = false, inPlace = false;
    (*drv)->WillDoIOOperation(drv, dev, 1, kAudioServerPlugInIOOperationWriteMix, &will, &inPlace);
    CHECK(will, "takes the output mix");
    float tone[512 * 2];
    for (int i = 0; i < 512; i++) tone[i * 2] = tone[i * 2 + 1] = 0.5f * sinf(i * 0.1f);
    AudioServerPlugInIOCycleInfo cycle = {0};
    cycle.mOutputTime.mSampleTime = 1000;
    CHECK((*drv)->DoIOOperation(drv, dev, 3, 1, kAudioServerPlugInIOOperationWriteMix, 512, &cycle, tone, NULL) == 0, "write mix");

    int fd = shm_open(SK_SHM_NAME, O_RDWR, 0);
    CHECK(fd >= 0, "app side opens the shared memory");
    if (fd >= 0) {
        SKRing *r = mmap(NULL, SKRingBytes(), PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        close(fd);
        CHECK(r != MAP_FAILED && r->magic == SK_RING_MAGIC, "ring header");
        if (r != MAP_FAILED) {
            uint64_t end = atomic_load(&r->write_end);
            int same = 1;
            for (int i = 0; i < 512; i++) if (r->data[((1000 + i) % SK_RING_FRAMES) * 2] != tone[i * 2]) same = 0;
            CHECK(end == 1512 && same, "tone arrived (write_end %llu)", end);
            CHECK(atomic_load(&r->running) == 1, "running flag set");
            uint32_t bits = atomic_load(&r->volume_bits); float rv; memcpy(&rv, &bits, 4);
            CHECK(fabsf(rv - 0.25f) < 1e-6, "volume shared with the app (%.2f)", rv);
        }
    }
    // Optional: keep playing a tone in real time, for testing the app's reader.
    if (argc > 3 && strcmp(argv[2], "--play-seconds") == 0) {
        // Like the sound server: on the driver's own clock, write blocks of N
        // frames one block ahead of "now" (N = the app's buffer size).
        double secs = atof(argv[3]);
        int N = argc > 4 ? atoi(argv[4]) : 512;
        mach_timebase_info_data_t tb; mach_timebase_info(&tb);
        double tpf = 1e9 * tb.denom / tb.numer / 48000.0;
        Float64 zs = 0; UInt64 zh = 0, zseed = 0;
        (*drv)->GetZeroTimeStamp(drv, dev, 1, &zs, &zh, &zseed);
        uint64_t t0 = mach_absolute_time();
        double next = floor(zs + (double)(t0 - zh) / tpf);
        float *buf = calloc((size_t)N * 2, sizeof(float));
        long blocks = 0;
        while ((double)(mach_absolute_time() - t0) * tb.numer / tb.denom / 1e9 < secs) {
            (*drv)->GetZeroTimeStamp(drv, dev, 1, &zs, &zh, &zseed);
            double now = zs + (double)(mach_absolute_time() - zh) / tpf;
            while (next <= now + N) {
                for (int i = 0; i < N; i++) buf[i * 2] = buf[i * 2 + 1] = 0.5f * sinf((float)fmod((next + i) * 2 * M_PI * 440.0 / 48000.0, 2 * M_PI));
                AudioServerPlugInIOCycleInfo c = {0};
                c.mOutputTime.mSampleTime = next;
                (*drv)->DoIOOperation(drv, dev, 3, 1, kAudioServerPlugInIOOperationWriteMix, (UInt32)N, &c, buf, NULL);
                next += N; blocks++;
            }
            usleep(2000);
        }
        free(buf);
        printf("played %ld blocks of %d frames in real time\n", blocks, N);
    }
    (*drv)->StopIO(drv, dev, 1);
    printf("%s\n", failures ? "SOME CHECKS FAILED" : "all checks passed");
    return failures ? 1 : 0;
}
