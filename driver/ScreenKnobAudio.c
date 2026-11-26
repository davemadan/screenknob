// ScreenKnob Audio: a virtual output device for macOS's sound server.
//
// It shows up as a speaker named "ScreenKnob" with a real volume and mute
// control, so macOS's own volume keys, popup and click work for it. Whatever
// apps play to it is copied into a shared-memory ring that the ScreenKnob app
// reads and plays on the screen. Nothing records sound, so macOS shows no
// recording indicator.
//
// Objects: the plug-in (1), the device (2), its output stream (3), the volume
// control (4) and the mute control (5). Fixed format: 48 kHz, 2-channel float.

#include <CoreAudio/AudioServerPlugIn.h>
#include <CoreFoundation/CoreFoundation.h>
#include <mach/mach_time.h>
#include <pthread.h>
#include <stdatomic.h>
#include <string.h>
#include <math.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>

#include "ScreenKnobRing.h"

// ---------------------------------------------------------------------------
// Identity

#define kPlugIn   kAudioObjectPlugInObject
#define kDevice   2
#define kStream   3
#define kVolume   4
#define kMute     5

#define kDeviceUID   "ScreenKnob_Device"
#define kModelUID    "ScreenKnob_Model"
#define kName        "ScreenKnob"
#define kMaker       "ScreenKnob"
#define kRate        48000.0
#define kChannels    2
#define kPeriod      16384          // frames between zero time stamps
#define kMinDB       (-64.0f)
#define kMaxDB       (0.0f)

// ---------------------------------------------------------------------------
// State

static pthread_mutex_t gLock = PTHREAD_MUTEX_INITIALIZER;
static AudioServerPlugInHostRef gHost = NULL;
static UInt32 gRefCount = 0;

static UInt32 gIOCount = 0;              // clients running IO
static UInt64 gAnchorHost = 0;           // host time of sample 0
static UInt64 gTickCount = 0;            // periods elapsed since the anchor
static Float64 gTicksPerFrame = 0;

static Float32 gVolume = 0.5f;           // scalar 0..1
static Boolean gMuted = false;

static SKRing *gRing = NULL;             // shared with the app

// ---------------------------------------------------------------------------
// Volume curve: amplitude = scalar², shown in dB, clamped to the range.

static Float32 ScalarToDB(Float32 s) {
    if (s <= 0.0f) return kMinDB;
    Float32 db = 20.0f * log10f(s * s);
    return db < kMinDB ? kMinDB : (db > kMaxDB ? kMaxDB : db);
}

static Float32 DBToScalar(Float32 db) {
    if (db <= kMinDB) return 0.0f;
    if (db > kMaxDB) db = kMaxDB;
    return sqrtf(powf(10.0f, db / 20.0f));
}

// ---------------------------------------------------------------------------
// Shared memory

static void OpenRing(void) {
    if (gRing) return;
    size_t size = SKRingBytes();
    int fd = shm_open(SK_SHM_NAME, O_CREAT | O_RDWR, 0666);
    if (fd < 0) return;
    fchmod(fd, 0666); // the app runs as you, the sound server as its own user
    struct stat st;
    if (fstat(fd, &st) == 0 && (size_t)st.st_size < size) {
        if (ftruncate(fd, (off_t)size) != 0) { close(fd); return; }
    }
    void *p = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (p == MAP_FAILED) return;
    SKRing *r = (SKRing *)p;
    if (r->magic != SK_RING_MAGIC || r->version != SK_RING_VERSION) {
        memset(p, 0, size);
        r->capacity = SK_RING_FRAMES;
        r->channels = kChannels;
        r->sample_rate = kRate;
        r->version = SK_RING_VERSION;
        r->magic = SK_RING_MAGIC;
    }
    gRing = r;
}

static void PublishLevel(void) {
    if (!gRing) return;
    atomic_store(&gRing->volume_bits, *(uint32_t *)&gVolume);
    atomic_store(&gRing->muted, gMuted ? 1u : 0u);
}

// ---------------------------------------------------------------------------
// Helpers for property answers

#define NEED(sz) do { if (inDataSize < (sz)) return kAudioHardwareBadPropertySizeError; } while (0)

static void Notify(AudioObjectID obj, AudioObjectPropertySelector a, AudioObjectPropertySelector b) {
    if (!gHost) return;
    AudioObjectPropertyAddress addrs[2] = {
        { a, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain },
        { b, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain },
    };
    gHost->PropertiesChanged(gHost, obj, b ? 2 : 1, addrs);
}

static AudioStreamBasicDescription Format(void) {
    AudioStreamBasicDescription f = {0};
    f.mSampleRate = kRate;
    f.mFormatID = kAudioFormatLinearPCM;
    f.mFormatFlags = kAudioFormatFlagIsFloat | kAudioFormatFlagsNativeEndian | kAudioFormatFlagIsPacked;
    f.mBytesPerPacket = 4 * kChannels;
    f.mFramesPerPacket = 1;
    f.mBytesPerFrame = 4 * kChannels;
    f.mChannelsPerFrame = kChannels;
    f.mBitsPerChannel = 32;
    return f;
}

// ---------------------------------------------------------------------------
// COM plumbing

static HRESULT SK_QueryInterface(void *, REFIID, LPVOID *);
static ULONG SK_AddRef(void *);
static ULONG SK_Release(void *);
static OSStatus SK_Initialize(AudioServerPlugInDriverRef, AudioServerPlugInHostRef);
static OSStatus SK_CreateDevice(AudioServerPlugInDriverRef, CFDictionaryRef, const AudioServerPlugInClientInfo *, AudioObjectID *);
static OSStatus SK_DestroyDevice(AudioServerPlugInDriverRef, AudioObjectID);
static OSStatus SK_AddDeviceClient(AudioServerPlugInDriverRef, AudioObjectID, const AudioServerPlugInClientInfo *);
static OSStatus SK_RemoveDeviceClient(AudioServerPlugInDriverRef, AudioObjectID, const AudioServerPlugInClientInfo *);
static OSStatus SK_PerformConfigChange(AudioServerPlugInDriverRef, AudioObjectID, UInt64, void *);
static OSStatus SK_AbortConfigChange(AudioServerPlugInDriverRef, AudioObjectID, UInt64, void *);
static Boolean SK_HasProperty(AudioServerPlugInDriverRef, AudioObjectID, pid_t, const AudioObjectPropertyAddress *);
static OSStatus SK_IsPropertySettable(AudioServerPlugInDriverRef, AudioObjectID, pid_t, const AudioObjectPropertyAddress *, Boolean *);
static OSStatus SK_GetPropertyDataSize(AudioServerPlugInDriverRef, AudioObjectID, pid_t, const AudioObjectPropertyAddress *, UInt32, const void *, UInt32 *);
static OSStatus SK_GetPropertyData(AudioServerPlugInDriverRef, AudioObjectID, pid_t, const AudioObjectPropertyAddress *, UInt32, const void *, UInt32, UInt32 *, void *);
static OSStatus SK_SetPropertyData(AudioServerPlugInDriverRef, AudioObjectID, pid_t, const AudioObjectPropertyAddress *, UInt32, const void *, UInt32, const void *);
static OSStatus SK_StartIO(AudioServerPlugInDriverRef, AudioObjectID, UInt32);
static OSStatus SK_StopIO(AudioServerPlugInDriverRef, AudioObjectID, UInt32);
static OSStatus SK_GetZeroTimeStamp(AudioServerPlugInDriverRef, AudioObjectID, UInt32, Float64 *, UInt64 *, UInt64 *);
static OSStatus SK_WillDoIOOperation(AudioServerPlugInDriverRef, AudioObjectID, UInt32, UInt32, Boolean *, Boolean *);
static OSStatus SK_BeginIOOperation(AudioServerPlugInDriverRef, AudioObjectID, UInt32, UInt32, UInt32, const AudioServerPlugInIOCycleInfo *);
static OSStatus SK_DoIOOperation(AudioServerPlugInDriverRef, AudioObjectID, AudioObjectID, UInt32, UInt32, UInt32, const AudioServerPlugInIOCycleInfo *, void *, void *);
static OSStatus SK_EndIOOperation(AudioServerPlugInDriverRef, AudioObjectID, UInt32, UInt32, UInt32, const AudioServerPlugInIOCycleInfo *);

static AudioServerPlugInDriverInterface gInterface = {
    NULL,
    SK_QueryInterface, SK_AddRef, SK_Release,
    SK_Initialize, SK_CreateDevice, SK_DestroyDevice,
    SK_AddDeviceClient, SK_RemoveDeviceClient,
    SK_PerformConfigChange, SK_AbortConfigChange,
    SK_HasProperty, SK_IsPropertySettable, SK_GetPropertyDataSize, SK_GetPropertyData, SK_SetPropertyData,
    SK_StartIO, SK_StopIO, SK_GetZeroTimeStamp,
    SK_WillDoIOOperation, SK_BeginIOOperation, SK_DoIOOperation, SK_EndIOOperation
};
static AudioServerPlugInDriverInterface *gInterfacePtr = &gInterface;
static AudioServerPlugInDriverRef gDriver = &gInterfacePtr;

// The factory named in Info.plist.
void *ScreenKnobAudio_Create(CFAllocatorRef allocator, CFUUIDRef typeUUID) {
    (void)allocator;
    if (CFEqual(typeUUID, kAudioServerPlugInTypeUUID)) return gDriver;
    return NULL;
}

static HRESULT SK_QueryInterface(void *driver, REFIID iid, LPVOID *out) {
    if (driver != gDriver || !out) return kAudioHardwareBadObjectError;
    CFUUIDRef want = CFUUIDCreateFromUUIDBytes(NULL, iid);
    HRESULT r = E_NOINTERFACE;
    if (CFEqual(want, IUnknownUUID) || CFEqual(want, kAudioServerPlugInDriverInterfaceUUID)) {
        pthread_mutex_lock(&gLock);
        gRefCount++;
        pthread_mutex_unlock(&gLock);
        *out = gDriver;
        r = S_OK;
    }
    CFRelease(want);
    return r;
}

static ULONG SK_AddRef(void *driver) {
    if (driver != gDriver) return 0;
    pthread_mutex_lock(&gLock);
    ULONG n = ++gRefCount;
    pthread_mutex_unlock(&gLock);
    return n;
}

static ULONG SK_Release(void *driver) {
    if (driver != gDriver) return 0;
    pthread_mutex_lock(&gLock);
    if (gRefCount > 0) gRefCount--;
    ULONG n = gRefCount;
    pthread_mutex_unlock(&gLock);
    return n;
}

static OSStatus SK_Initialize(AudioServerPlugInDriverRef driver, AudioServerPlugInHostRef host) {
    if (driver != gDriver) return kAudioHardwareBadObjectError;
    gHost = host;
    mach_timebase_info_data_t tb;
    mach_timebase_info(&tb);
    Float64 ticksPerSecond = 1e9 * (Float64)tb.denom / (Float64)tb.numer;
    gTicksPerFrame = ticksPerSecond / kRate;
    // Remember the level between restarts of the sound server.
    if (gHost && gHost->CopyFromStorage) {
        CFPropertyListRef v = NULL;
        if (gHost->CopyFromStorage(gHost, CFSTR("volume"), &v) == 0 && v) {
            if (CFGetTypeID(v) == CFNumberGetTypeID()) CFNumberGetValue((CFNumberRef)v, kCFNumberFloat32Type, &gVolume);
            CFRelease(v);
        }
        v = NULL;
        if (gHost->CopyFromStorage(gHost, CFSTR("muted"), &v) == 0 && v) {
            if (CFGetTypeID(v) == CFBooleanGetTypeID()) gMuted = CFBooleanGetValue((CFBooleanRef)v);
            CFRelease(v);
        }
    }
    OpenRing();
    PublishLevel();
    return 0;
}

static void StoreLevel(void) {
    if (!gHost || !gHost->WriteToStorage) return;
    CFNumberRef n = CFNumberCreate(NULL, kCFNumberFloat32Type, &gVolume);
    if (n) { gHost->WriteToStorage(gHost, CFSTR("volume"), n); CFRelease(n); }
    gHost->WriteToStorage(gHost, CFSTR("muted"), gMuted ? kCFBooleanTrue : kCFBooleanFalse);
}

static OSStatus SK_CreateDevice(AudioServerPlugInDriverRef d, CFDictionaryRef desc, const AudioServerPlugInClientInfo *c, AudioObjectID *out) {
    (void)d; (void)desc; (void)c; (void)out;
    return kAudioHardwareUnsupportedOperationError;
}
static OSStatus SK_DestroyDevice(AudioServerPlugInDriverRef d, AudioObjectID id) {
    (void)d; (void)id;
    return kAudioHardwareUnsupportedOperationError;
}
static OSStatus SK_AddDeviceClient(AudioServerPlugInDriverRef d, AudioObjectID id, const AudioServerPlugInClientInfo *c) {
    (void)d; (void)c;
    return id == kDevice ? 0 : kAudioHardwareBadObjectError;
}
static OSStatus SK_RemoveDeviceClient(AudioServerPlugInDriverRef d, AudioObjectID id, const AudioServerPlugInClientInfo *c) {
    (void)d; (void)c;
    return id == kDevice ? 0 : kAudioHardwareBadObjectError;
}
static OSStatus SK_PerformConfigChange(AudioServerPlugInDriverRef d, AudioObjectID id, UInt64 a, void *i) {
    (void)d; (void)id; (void)a; (void)i;
    return 0;
}
static OSStatus SK_AbortConfigChange(AudioServerPlugInDriverRef d, AudioObjectID id, UInt64 a, void *i) {
    (void)d; (void)id; (void)a; (void)i;
    return 0;
}

// ---------------------------------------------------------------------------
// Properties

static Boolean SK_HasProperty(AudioServerPlugInDriverRef d, AudioObjectID obj, pid_t pid, const AudioObjectPropertyAddress *a) {
    UInt32 size = 0;
    (void)d;
    return SK_GetPropertyDataSize(gDriver, obj, pid, a, 0, NULL, &size) == 0;
}

static OSStatus SK_IsPropertySettable(AudioServerPlugInDriverRef d, AudioObjectID obj, pid_t pid, const AudioObjectPropertyAddress *a, Boolean *out) {
    (void)d; (void)pid;
    if (!a || !out) return kAudioHardwareIllegalOperationError;
    UInt32 size = 0;
    if (SK_GetPropertyDataSize(gDriver, obj, pid, a, 0, NULL, &size) != 0) return kAudioHardwareUnknownPropertyError;
    switch (obj) {
        case kVolume:
            *out = a->mSelector == kAudioLevelControlPropertyScalarValue || a->mSelector == kAudioLevelControlPropertyDecibelValue;
            break;
        case kMute:
            *out = a->mSelector == kAudioBooleanControlPropertyValue;
            break;
        case kStream:
            *out = a->mSelector == kAudioStreamPropertyIsActive;
            break;
        default:
            *out = false;
    }
    return 0;
}

static Boolean OutputScope(const AudioObjectPropertyAddress *a) {
    return a->mScope == kAudioObjectPropertyScopeGlobal || a->mScope == kAudioObjectPropertyScopeOutput;
}

static OSStatus SK_GetPropertyDataSize(AudioServerPlugInDriverRef d, AudioObjectID obj, pid_t pid, const AudioObjectPropertyAddress *a, UInt32 qs, const void *q, UInt32 *out) {
    (void)d; (void)pid; (void)qs; (void)q;
    if (!a || !out) return kAudioHardwareIllegalOperationError;
    UInt32 n = 0;
    switch (obj) {
    case kPlugIn:
        switch (a->mSelector) {
            case kAudioObjectPropertyBaseClass: case kAudioObjectPropertyClass: n = sizeof(AudioClassID); break;
            case kAudioObjectPropertyOwner: n = sizeof(AudioObjectID); break;
            case kAudioObjectPropertyManufacturer: n = sizeof(CFStringRef); break;
            case kAudioObjectPropertyOwnedObjects: case kAudioPlugInPropertyDeviceList: n = sizeof(AudioObjectID); break;
            case kAudioPlugInPropertyTranslateUIDToDevice: n = sizeof(AudioObjectID); break;
            case kAudioPlugInPropertyResourceBundle: n = sizeof(CFStringRef); break;
            default: return kAudioHardwareUnknownPropertyError;
        }
        break;
    case kDevice:
        switch (a->mSelector) {
            case kAudioObjectPropertyBaseClass: case kAudioObjectPropertyClass: n = sizeof(AudioClassID); break;
            case kAudioObjectPropertyOwner: n = sizeof(AudioObjectID); break;
            case kAudioObjectPropertyName: case kAudioObjectPropertyManufacturer:
            case kAudioDevicePropertyDeviceUID: case kAudioDevicePropertyModelUID: n = sizeof(CFStringRef); break;
            case kAudioObjectPropertyOwnedObjects:
                n = (a->mScope == kAudioObjectPropertyScopeInput ? 0 : 3) * sizeof(AudioObjectID); break;
            case kAudioDevicePropertyTransportType: case kAudioDevicePropertyClockDomain:
            case kAudioDevicePropertyDeviceIsAlive: case kAudioDevicePropertyDeviceIsRunning:
            case kAudioDevicePropertyDeviceCanBeDefaultDevice: case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice:
            case kAudioDevicePropertyLatency: case kAudioDevicePropertySafetyOffset:
            case kAudioDevicePropertyIsHidden: case kAudioDevicePropertyZeroTimeStampPeriod:
                n = sizeof(UInt32); break;
            case kAudioDevicePropertyRelatedDevices: n = sizeof(AudioObjectID); break;
            case kAudioDevicePropertyStreams:
                n = (OutputScope(a) ? 1 : 0) * sizeof(AudioObjectID); break;
            case kAudioObjectPropertyControlList: n = 2 * sizeof(AudioObjectID); break;
            case kAudioDevicePropertyNominalSampleRate: n = sizeof(Float64); break;
            case kAudioDevicePropertyAvailableNominalSampleRates: n = sizeof(AudioValueRange); break;
            case kAudioDevicePropertyPreferredChannelsForStereo: n = 2 * sizeof(UInt32); break;
            case kAudioDevicePropertyPreferredChannelLayout:
                n = (UInt32)(offsetof(AudioChannelLayout, mChannelDescriptions) + kChannels * sizeof(AudioChannelDescription)); break;
            default: return kAudioHardwareUnknownPropertyError;
        }
        break;
    case kStream:
        switch (a->mSelector) {
            case kAudioObjectPropertyBaseClass: case kAudioObjectPropertyClass: n = sizeof(AudioClassID); break;
            case kAudioObjectPropertyOwner: n = sizeof(AudioObjectID); break;
            case kAudioObjectPropertyOwnedObjects: n = 0; break;
            case kAudioObjectPropertyName: n = sizeof(CFStringRef); break;
            case kAudioStreamPropertyIsActive: case kAudioStreamPropertyDirection: case kAudioStreamPropertyTerminalType:
            case kAudioStreamPropertyStartingChannel: case kAudioStreamPropertyLatency: n = sizeof(UInt32); break;
            case kAudioStreamPropertyVirtualFormat: case kAudioStreamPropertyPhysicalFormat: n = sizeof(AudioStreamBasicDescription); break;
            case kAudioStreamPropertyAvailableVirtualFormats: case kAudioStreamPropertyAvailablePhysicalFormats:
                n = sizeof(AudioStreamRangedDescription); break;
            default: return kAudioHardwareUnknownPropertyError;
        }
        break;
    case kVolume:
        switch (a->mSelector) {
            case kAudioObjectPropertyBaseClass: case kAudioObjectPropertyClass: n = sizeof(AudioClassID); break;
            case kAudioObjectPropertyOwner: n = sizeof(AudioObjectID); break;
            case kAudioObjectPropertyOwnedObjects: n = 0; break;
            case kAudioControlPropertyScope: case kAudioControlPropertyElement: n = sizeof(UInt32); break;
            case kAudioLevelControlPropertyScalarValue: case kAudioLevelControlPropertyDecibelValue:
            case kAudioLevelControlPropertyConvertScalarToDecibels: case kAudioLevelControlPropertyConvertDecibelsToScalar:
                n = sizeof(Float32); break;
            case kAudioLevelControlPropertyDecibelRange: n = sizeof(AudioValueRange); break;
            default: return kAudioHardwareUnknownPropertyError;
        }
        break;
    case kMute:
        switch (a->mSelector) {
            case kAudioObjectPropertyBaseClass: case kAudioObjectPropertyClass: n = sizeof(AudioClassID); break;
            case kAudioObjectPropertyOwner: n = sizeof(AudioObjectID); break;
            case kAudioObjectPropertyOwnedObjects: n = 0; break;
            case kAudioControlPropertyScope: case kAudioControlPropertyElement:
            case kAudioBooleanControlPropertyValue: n = sizeof(UInt32); break;
            default: return kAudioHardwareUnknownPropertyError;
        }
        break;
    default:
        return kAudioHardwareBadObjectError;
    }
    *out = n;
    return 0;
}

#define PUT(type, value) do { NEED(sizeof(type)); *(type *)out = (value); *outSize = sizeof(type); } while (0)

static OSStatus SK_GetPropertyData(AudioServerPlugInDriverRef d, AudioObjectID obj, pid_t pid, const AudioObjectPropertyAddress *a,
                                   UInt32 qs, const void *q, UInt32 inDataSize, UInt32 *outSize, void *out) {
    (void)d; (void)pid;
    if (!a || !outSize || !out) return kAudioHardwareIllegalOperationError;
    switch (obj) {
    case kPlugIn:
        switch (a->mSelector) {
            case kAudioObjectPropertyBaseClass: PUT(AudioClassID, kAudioObjectClassID); return 0;
            case kAudioObjectPropertyClass: PUT(AudioClassID, kAudioPlugInClassID); return 0;
            case kAudioObjectPropertyOwner: PUT(AudioObjectID, kAudioObjectUnknown); return 0;
            case kAudioObjectPropertyManufacturer: PUT(CFStringRef, CFSTR(kMaker)); return 0;
            case kAudioObjectPropertyOwnedObjects:
            case kAudioPlugInPropertyDeviceList:
                if (inDataSize < sizeof(AudioObjectID)) { *outSize = 0; return 0; }
                *(AudioObjectID *)out = kDevice; *outSize = sizeof(AudioObjectID); return 0;
            case kAudioPlugInPropertyTranslateUIDToDevice: {
                NEED(sizeof(AudioObjectID));
                AudioObjectID r = kAudioObjectUnknown;
                if (q && qs == sizeof(CFStringRef) && *(CFStringRef const *)q &&
                    CFStringCompare(*(CFStringRef const *)q, CFSTR(kDeviceUID), 0) == kCFCompareEqualTo) r = kDevice;
                *(AudioObjectID *)out = r; *outSize = sizeof(AudioObjectID); return 0;
            }
            case kAudioPlugInPropertyResourceBundle: PUT(CFStringRef, CFSTR("")); return 0;
        }
        return kAudioHardwareUnknownPropertyError;

    case kDevice:
        switch (a->mSelector) {
            case kAudioObjectPropertyBaseClass: PUT(AudioClassID, kAudioObjectClassID); return 0;
            case kAudioObjectPropertyClass: PUT(AudioClassID, kAudioDeviceClassID); return 0;
            case kAudioObjectPropertyOwner: PUT(AudioObjectID, kPlugIn); return 0;
            case kAudioObjectPropertyName: PUT(CFStringRef, CFSTR(kName)); return 0;
            case kAudioObjectPropertyManufacturer: PUT(CFStringRef, CFSTR(kMaker)); return 0;
            case kAudioDevicePropertyDeviceUID: PUT(CFStringRef, CFSTR(kDeviceUID)); return 0;
            case kAudioDevicePropertyModelUID: PUT(CFStringRef, CFSTR(kModelUID)); return 0;
            case kAudioObjectPropertyOwnedObjects: {
                AudioObjectID ids[3] = { kStream, kVolume, kMute };
                UInt32 count = a->mScope == kAudioObjectPropertyScopeInput ? 0 : 3;
                UInt32 fit = inDataSize / sizeof(AudioObjectID);
                if (fit < count) count = fit;
                memcpy(out, ids, count * sizeof(AudioObjectID));
                *outSize = count * sizeof(AudioObjectID); return 0;
            }
            case kAudioDevicePropertyTransportType: PUT(UInt32, kAudioDeviceTransportTypeVirtual); return 0;
            case kAudioDevicePropertyRelatedDevices:
                if (inDataSize < sizeof(AudioObjectID)) { *outSize = 0; return 0; }
                *(AudioObjectID *)out = kDevice; *outSize = sizeof(AudioObjectID); return 0;
            case kAudioDevicePropertyClockDomain: PUT(UInt32, 0); return 0;
            case kAudioDevicePropertyDeviceIsAlive: PUT(UInt32, 1); return 0;
            case kAudioDevicePropertyDeviceIsRunning: {
                pthread_mutex_lock(&gLock);
                UInt32 running = gIOCount > 0;
                pthread_mutex_unlock(&gLock);
                PUT(UInt32, running); return 0;
            }
            case kAudioDevicePropertyDeviceCanBeDefaultDevice:
                PUT(UInt32, a->mScope == kAudioObjectPropertyScopeInput ? 0 : 1); return 0;
            case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice: PUT(UInt32, 1); return 0;
            case kAudioDevicePropertyLatency: PUT(UInt32, 0); return 0;
            case kAudioDevicePropertySafetyOffset: PUT(UInt32, 0); return 0;
            case kAudioDevicePropertyIsHidden: PUT(UInt32, 0); return 0;
            case kAudioDevicePropertyZeroTimeStampPeriod: PUT(UInt32, kPeriod); return 0;
            case kAudioDevicePropertyStreams:
                if (!OutputScope(a) || inDataSize < sizeof(AudioObjectID)) { *outSize = 0; return 0; }
                *(AudioObjectID *)out = kStream; *outSize = sizeof(AudioObjectID); return 0;
            case kAudioObjectPropertyControlList: {
                AudioObjectID ids[2] = { kVolume, kMute };
                UInt32 count = inDataSize / sizeof(AudioObjectID);
                if (count > 2) count = 2;
                memcpy(out, ids, count * sizeof(AudioObjectID));
                *outSize = count * sizeof(AudioObjectID); return 0;
            }
            case kAudioDevicePropertyNominalSampleRate: PUT(Float64, kRate); return 0;
            case kAudioDevicePropertyAvailableNominalSampleRates: {
                if (inDataSize < sizeof(AudioValueRange)) { *outSize = 0; return 0; }
                AudioValueRange r = { kRate, kRate };
                *(AudioValueRange *)out = r; *outSize = sizeof(AudioValueRange); return 0;
            }
            case kAudioDevicePropertyPreferredChannelsForStereo: {
                NEED(2 * sizeof(UInt32));
                ((UInt32 *)out)[0] = 1; ((UInt32 *)out)[1] = 2;
                *outSize = 2 * sizeof(UInt32); return 0;
            }
            case kAudioDevicePropertyPreferredChannelLayout: {
                UInt32 size = (UInt32)(offsetof(AudioChannelLayout, mChannelDescriptions) + kChannels * sizeof(AudioChannelDescription));
                NEED(size);
                AudioChannelLayout *l = (AudioChannelLayout *)out;
                memset(l, 0, size);
                l->mChannelLayoutTag = kAudioChannelLayoutTag_UseChannelDescriptions;
                l->mNumberChannelDescriptions = kChannels;
                l->mChannelDescriptions[0].mChannelLabel = kAudioChannelLabel_Left;
                l->mChannelDescriptions[1].mChannelLabel = kAudioChannelLabel_Right;
                *outSize = size; return 0;
            }
        }
        return kAudioHardwareUnknownPropertyError;

    case kStream:
        switch (a->mSelector) {
            case kAudioObjectPropertyBaseClass: PUT(AudioClassID, kAudioObjectClassID); return 0;
            case kAudioObjectPropertyClass: PUT(AudioClassID, kAudioStreamClassID); return 0;
            case kAudioObjectPropertyOwner: PUT(AudioObjectID, kDevice); return 0;
            case kAudioObjectPropertyOwnedObjects: *outSize = 0; return 0;
            case kAudioObjectPropertyName: PUT(CFStringRef, CFSTR("ScreenKnob Output")); return 0;
            case kAudioStreamPropertyIsActive: PUT(UInt32, 1); return 0;
            case kAudioStreamPropertyDirection: PUT(UInt32, 0); return 0;
            case kAudioStreamPropertyTerminalType: PUT(UInt32, kAudioStreamTerminalTypeSpeaker); return 0;
            case kAudioStreamPropertyStartingChannel: PUT(UInt32, 1); return 0;
            case kAudioStreamPropertyLatency: PUT(UInt32, 0); return 0;
            case kAudioStreamPropertyVirtualFormat:
            case kAudioStreamPropertyPhysicalFormat: PUT(AudioStreamBasicDescription, Format()); return 0;
            case kAudioStreamPropertyAvailableVirtualFormats:
            case kAudioStreamPropertyAvailablePhysicalFormats: {
                if (inDataSize < sizeof(AudioStreamRangedDescription)) { *outSize = 0; return 0; }
                AudioStreamRangedDescription r;
                r.mFormat = Format();
                r.mSampleRateRange.mMinimum = kRate;
                r.mSampleRateRange.mMaximum = kRate;
                *(AudioStreamRangedDescription *)out = r;
                *outSize = sizeof(AudioStreamRangedDescription); return 0;
            }
        }
        return kAudioHardwareUnknownPropertyError;

    case kVolume:
        switch (a->mSelector) {
            case kAudioObjectPropertyBaseClass: PUT(AudioClassID, kAudioLevelControlClassID); return 0;
            case kAudioObjectPropertyClass: PUT(AudioClassID, kAudioVolumeControlClassID); return 0;
            case kAudioObjectPropertyOwner: PUT(AudioObjectID, kDevice); return 0;
            case kAudioObjectPropertyOwnedObjects: *outSize = 0; return 0;
            case kAudioControlPropertyScope: PUT(UInt32, kAudioObjectPropertyScopeOutput); return 0;
            case kAudioControlPropertyElement: PUT(UInt32, kAudioObjectPropertyElementMain); return 0;
            case kAudioLevelControlPropertyScalarValue: {
                pthread_mutex_lock(&gLock); Float32 v = gVolume; pthread_mutex_unlock(&gLock);
                PUT(Float32, v); return 0;
            }
            case kAudioLevelControlPropertyDecibelValue: {
                pthread_mutex_lock(&gLock); Float32 v = gVolume; pthread_mutex_unlock(&gLock);
                PUT(Float32, ScalarToDB(v)); return 0;
            }
            case kAudioLevelControlPropertyDecibelRange: {
                NEED(sizeof(AudioValueRange));
                AudioValueRange r = { kMinDB, kMaxDB };
                *(AudioValueRange *)out = r; *outSize = sizeof(AudioValueRange); return 0;
            }
            case kAudioLevelControlPropertyConvertScalarToDecibels:
                NEED(sizeof(Float32)); *(Float32 *)out = ScalarToDB(*(Float32 *)out); *outSize = sizeof(Float32); return 0;
            case kAudioLevelControlPropertyConvertDecibelsToScalar:
                NEED(sizeof(Float32)); *(Float32 *)out = DBToScalar(*(Float32 *)out); *outSize = sizeof(Float32); return 0;
        }
        return kAudioHardwareUnknownPropertyError;

    case kMute:
        switch (a->mSelector) {
            case kAudioObjectPropertyBaseClass: PUT(AudioClassID, kAudioBooleanControlClassID); return 0;
            case kAudioObjectPropertyClass: PUT(AudioClassID, kAudioMuteControlClassID); return 0;
            case kAudioObjectPropertyOwner: PUT(AudioObjectID, kDevice); return 0;
            case kAudioObjectPropertyOwnedObjects: *outSize = 0; return 0;
            case kAudioControlPropertyScope: PUT(UInt32, kAudioObjectPropertyScopeOutput); return 0;
            case kAudioControlPropertyElement: PUT(UInt32, kAudioObjectPropertyElementMain); return 0;
            case kAudioBooleanControlPropertyValue: {
                pthread_mutex_lock(&gLock); UInt32 m = gMuted ? 1 : 0; pthread_mutex_unlock(&gLock);
                PUT(UInt32, m); return 0;
            }
        }
        return kAudioHardwareUnknownPropertyError;
    }
    return kAudioHardwareBadObjectError;
}

static OSStatus SK_SetPropertyData(AudioServerPlugInDriverRef d, AudioObjectID obj, pid_t pid, const AudioObjectPropertyAddress *a,
                                   UInt32 qs, const void *q, UInt32 inDataSize, const void *in) {
    (void)d; (void)pid; (void)qs; (void)q;
    if (!a || !in) return kAudioHardwareIllegalOperationError;
    if (obj == kVolume && (a->mSelector == kAudioLevelControlPropertyScalarValue || a->mSelector == kAudioLevelControlPropertyDecibelValue)) {
        if (inDataSize != sizeof(Float32)) return kAudioHardwareBadPropertySizeError;
        Float32 v = *(const Float32 *)in;
        if (a->mSelector == kAudioLevelControlPropertyDecibelValue) v = DBToScalar(v);
        if (v < 0.0f) v = 0.0f;
        if (v > 1.0f) v = 1.0f;
        pthread_mutex_lock(&gLock);
        Boolean changed = v != gVolume;
        gVolume = v;
        PublishLevel();
        pthread_mutex_unlock(&gLock);
        if (changed) {
            StoreLevel();
            Notify(kVolume, kAudioLevelControlPropertyScalarValue, kAudioLevelControlPropertyDecibelValue);
        }
        return 0;
    }
    if (obj == kMute && a->mSelector == kAudioBooleanControlPropertyValue) {
        if (inDataSize != sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
        Boolean m = *(const UInt32 *)in != 0;
        pthread_mutex_lock(&gLock);
        Boolean changed = m != gMuted;
        gMuted = m;
        PublishLevel();
        pthread_mutex_unlock(&gLock);
        if (changed) {
            StoreLevel();
            Notify(kMute, kAudioBooleanControlPropertyValue, 0);
        }
        return 0;
    }
    if (obj == kStream && a->mSelector == kAudioStreamPropertyIsActive) return 0;
    return kAudioHardwareUnknownPropertyError;
}

// ---------------------------------------------------------------------------
// IO

static OSStatus SK_StartIO(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client) {
    (void)d; (void)client;
    if (id != kDevice) return kAudioHardwareBadObjectError;
    pthread_mutex_lock(&gLock);
    if (gIOCount == 0) {
        gAnchorHost = mach_absolute_time();
        gTickCount = 0;
        OpenRing();
        if (gRing) {
            atomic_store(&gRing->running, 1u);
            atomic_fetch_add(&gRing->generation, 1u);
        }
    }
    gIOCount++;
    pthread_mutex_unlock(&gLock);
    return 0;
}

static OSStatus SK_StopIO(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client) {
    (void)d; (void)client;
    if (id != kDevice) return kAudioHardwareBadObjectError;
    pthread_mutex_lock(&gLock);
    if (gIOCount > 0) gIOCount--;
    if (gIOCount == 0 && gRing) atomic_store(&gRing->running, 0u);
    pthread_mutex_unlock(&gLock);
    return 0;
}

static OSStatus SK_GetZeroTimeStamp(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client,
                                    Float64 *outSample, UInt64 *outHost, UInt64 *outSeed) {
    (void)d; (void)client;
    if (id != kDevice) return kAudioHardwareBadObjectError;
    pthread_mutex_lock(&gLock);
    Float64 ticksPerPeriod = gTicksPerFrame * kPeriod;
    UInt64 now = mach_absolute_time();
    // Step forward whole periods until the next one is still ahead.
    while ((Float64)(now - gAnchorHost) >= (Float64)(gTickCount + 1) * ticksPerPeriod) gTickCount++;
    *outSample = (Float64)gTickCount * kPeriod;
    *outHost = gAnchorHost + (UInt64)((Float64)gTickCount * ticksPerPeriod);
    *outSeed = 1;
    pthread_mutex_unlock(&gLock);
    return 0;
}

static OSStatus SK_WillDoIOOperation(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client, UInt32 op, Boolean *will, Boolean *inPlace) {
    (void)d; (void)client;
    if (id != kDevice) return kAudioHardwareBadObjectError;
    if (will) *will = op == kAudioServerPlugInIOOperationWriteMix;
    if (inPlace) *inPlace = true;
    return 0;
}

static OSStatus SK_BeginIOOperation(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 c, UInt32 op, UInt32 n, const AudioServerPlugInIOCycleInfo *i) {
    (void)d; (void)c; (void)op; (void)n; (void)i;
    return id == kDevice ? 0 : kAudioHardwareBadObjectError;
}

static OSStatus SK_DoIOOperation(AudioServerPlugInDriverRef d, AudioObjectID id, AudioObjectID stream, UInt32 client, UInt32 op,
                                 UInt32 frames, const AudioServerPlugInIOCycleInfo *cycle, void *main, void *secondary) {
    (void)d; (void)client; (void)secondary;
    if (id != kDevice || stream != kStream) return kAudioHardwareBadObjectError;
    if (op != kAudioServerPlugInIOOperationWriteMix || !main || !cycle || !gRing) return 0;
    // Copy the mixed output into the ring at its sample time; the app plays it.
    UInt64 start = (UInt64)cycle->mOutputTime.mSampleTime;
    const Float32 *src = (const Float32 *)main;
    for (UInt32 f = 0; f < frames; f++) {
        UInt64 slot = (start + f) % SK_RING_FRAMES;
        gRing->data[slot * kChannels + 0] = src[f * kChannels + 0];
        gRing->data[slot * kChannels + 1] = src[f * kChannels + 1];
    }
    atomic_store_explicit(&gRing->write_end, start + frames, memory_order_release);
    return 0;
}

static OSStatus SK_EndIOOperation(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 c, UInt32 op, UInt32 n, const AudioServerPlugInIOCycleInfo *i) {
    (void)d; (void)c; (void)op; (void)n; (void)i;
    return id == kDevice ? 0 : kAudioHardwareBadObjectError;
}
