#include <os/log.h>
#include <string.h>
#include <DriverKit/IOLib.h>

#include "ExtenderVirtualAudio.h"
#include "ExtenderProtocol.h"

#define LOG_PREFIX "ExtenderAudio"

namespace {

struct PendingOutputEntry {
    uint8_t  data[EXTENDER_MAX_TRANSFER_SIZE];
    uint32_t length;
    uint32_t endpoint;
    uint32_t requestType;
    bool     valid;
};

constexpr uint32_t kPendingOutputCapacity = EXTENDER_MAX_PENDING_OUTPUTS;

}  // namespace

struct ExtenderVirtualAudio_IVars {
    uint32_t sampleRate;
    uint16_t channels;
    uint16_t bitDepth;

    PendingOutputEntry pending[kPendingOutputCapacity];
    uint32_t           pendingHead;
    uint32_t           pendingTail;
    uint32_t           pendingCount;

    bool started;
};

bool ExtenderVirtualAudio::init()
{
    if (!super::init()) return false;
    ivars = IONewZero(ExtenderVirtualAudio_IVars, 1);
    if (!ivars) return false;
    ivars->sampleRate = 48000;
    ivars->channels = 2;
    ivars->bitDepth = 16;
    ivars->started = false;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": init");
    return true;
}

kern_return_t IMPL(ExtenderVirtualAudio, Start)
{
    kern_return_t ret = Start(provider, SUPERDISPATCH);
    if (ret != kIOReturnSuccess) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Start super failed: 0x%x", ret);
        return ret;
    }
    ivars->started = true;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Started (%uHz %uch %ubit)",
           ivars->sampleRate, ivars->channels, ivars->bitDepth);
    // NOTE: IOUserAudioDevice::Create(this, ...) creates an audio device this
    // driver owns. Skipped for scaffold — needs IOUserAudioStream + format
    // negotiation + control objects which is its own substantial piece.
    RegisterService();
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualAudio, Stop)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Stop");
    ivars->started = false;
    IOSafeDeleteNULL(ivars, ExtenderVirtualAudio_IVars, 1);
    return Stop(provider, SUPERDISPATCH);
}

static bool enqueuePending(ExtenderVirtualAudio_IVars *ivars,
                           uint32_t requestType,
                           const void *bytes,
                           uint32_t length)
{
    if (length > EXTENDER_MAX_TRANSFER_SIZE) return false;
    if (ivars->pendingCount >= kPendingOutputCapacity) return false;
    PendingOutputEntry &entry = ivars->pending[ivars->pendingTail];
    entry.requestType = requestType;
    entry.endpoint = 0;
    entry.length = length;
    if (length > 0 && bytes) memcpy(entry.data, bytes, length);
    entry.valid = true;
    ivars->pendingTail = (ivars->pendingTail + 1) % kPendingOutputCapacity;
    ivars->pendingCount++;
    return true;
}

void ExtenderVirtualAudio::submitInputReport(const void *data, uint32_t length)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": submitInputReport len=%u (drop — audio stream TODO)", length);
    (void)data;
}

bool ExtenderVirtualAudio::dequeuePendingOutput(void *hdrOut, void *bufOut, uint32_t maxLen, uint32_t *outLen)
{
    if (!ivars || !hdrOut || !outLen) return false;
    if (ivars->pendingCount == 0) { *outLen = 0; return false; }
    PendingOutputEntry &entry = ivars->pending[ivars->pendingHead];
    ExtenderPendingOutputHeader *hdr = (ExtenderPendingOutputHeader *)hdrOut;
    hdr->deviceId = 0;
    hdr->endpoint = entry.endpoint;
    hdr->requestType = entry.requestType;
    uint32_t copyLen = entry.length < maxLen ? entry.length : maxLen;
    hdr->dataLength = copyLen;
    if (copyLen > 0 && bufOut) memcpy(bufOut, entry.data, copyLen);
    *outLen = copyLen;
    entry.valid = false;
    ivars->pendingHead = (ivars->pendingHead + 1) % kPendingOutputCapacity;
    ivars->pendingCount--;
    return true;
}
