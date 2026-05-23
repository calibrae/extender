#include <os/log.h>
#include <string.h>
#include <DriverKit/IOLib.h>
#include <DriverKit/IOService.h>
#include <DriverKit/OSData.h>

#include "ExtenderVirtualStorage.h"
#include "ExtenderProtocol.h"

#define LOG_PREFIX "ExtenderStorage"

namespace {

struct PendingOutputEntry {
    uint8_t  data[EXTENDER_MAX_TRANSFER_SIZE];
    uint32_t length;
    uint32_t endpoint;
    uint32_t requestType;
    uint32_t requestId;
    bool     valid;
};

struct InFlightIO {
    uint32_t requestId;     // OS-provided request id passed to DoAsyncReadWrite
    uint64_t dmaAddr;       // DMA address of the OS's buffer
    uint64_t expectedSize;  // Size we expected to transfer
    bool     isRead;
    bool     valid;
};

constexpr uint32_t kPendingOutputCapacity = EXTENDER_MAX_PENDING_OUTPUTS;
constexpr uint32_t kInFlightCapacity      = 16;

}  // namespace

struct ExtenderVirtualStorage_IVars {
    uint64_t blockCount;
    uint32_t blockSize;

    char     vendor[kMaxDeviceStringLength];
    char     product[kMaxDeviceStringLength];
    char     revision[kMaxDeviceStringLength];

    PendingOutputEntry pending[kPendingOutputCapacity];
    uint32_t           pendingHead;
    uint32_t           pendingTail;
    uint32_t           pendingCount;

    InFlightIO inFlight[kInFlightCapacity];

    bool     started;
};

bool ExtenderVirtualStorage::init()
{
    if (!super::init()) return false;

    ivars = IONewZero(ExtenderVirtualStorage_IVars, 1);
    if (!ivars) return false;

    ivars->blockCount = 0;
    ivars->blockSize = 512;
    strncpy(ivars->vendor, "Extender", sizeof(ivars->vendor) - 1);
    strncpy(ivars->product, "Virtual Block", sizeof(ivars->product) - 1);
    strncpy(ivars->revision, "0001", sizeof(ivars->revision) - 1);
    ivars->started = false;

    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": init");
    return true;
}

kern_return_t IMPL(ExtenderVirtualStorage, Start)
{
    kern_return_t ret = Start(provider, SUPERDISPATCH);
    if (ret != kIOReturnSuccess) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Start super failed: 0x%x", ret);
        return ret;
    }
    ivars->started = true;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Started (blocks=%llu size=%u)", ivars->blockCount, ivars->blockSize);
    RegisterDext();
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualStorage, Stop)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Stop");
    ivars->started = false;
    IOSafeDeleteNULL(ivars, ExtenderVirtualStorage_IVars, 1);
    return Stop(provider, SUPERDISPATCH);
}

kern_return_t IMPL(ExtenderVirtualStorage, GetDeviceParams)
{
    if (!deviceParams) return kIOReturnBadArgument;
    deviceParams->numOfBlocks = ivars->blockCount > 0 ? ivars->blockCount : 1;
    deviceParams->blockSize = ivars->blockSize > 0 ? ivars->blockSize : 512;
    deviceParams->maxIOSize = EXTENDER_MAX_TRANSFER_SIZE;
    deviceParams->numOfOutstandingIOs = 16;
    deviceParams->maxNumOfUnmapRegions = 0;
    deviceParams->minSegmentAlignment = 1;
    deviceParams->numOfAddressBits = 64;
    deviceParams->isUnmapSupported = false;
    deviceParams->isFUASupported = false;
    return kIOReturnSuccess;
}

static kern_return_t copyDeviceString(DeviceString *dst, const char *src)
{
    if (!dst) return kIOReturnBadArgument;
    memset(dst->data, 0, sizeof(dst->data));
    if (src) strncpy(dst->data, src, sizeof(dst->data) - 1);
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualStorage, GetVendorString)
{ return copyDeviceString(vendor, ivars->vendor); }

kern_return_t IMPL(ExtenderVirtualStorage, GetProductString)
{ return copyDeviceString(product, ivars->product); }

kern_return_t IMPL(ExtenderVirtualStorage, GetRevisionString)
{ return copyDeviceString(revision, ivars->revision); }

kern_return_t IMPL(ExtenderVirtualStorage, GetAdditionalInfoString)
{ return copyDeviceString(additionalInfo, "Extender USB/IP"); }

kern_return_t IMPL(ExtenderVirtualStorage, ReportEjectability)
{ if (!isEjectable) return kIOReturnBadArgument; *isEjectable = true; return kIOReturnSuccess; }

kern_return_t IMPL(ExtenderVirtualStorage, ReportRemovability)
{ if (!isRemovable) return kIOReturnBadArgument; *isRemovable = true; return kIOReturnSuccess; }

kern_return_t IMPL(ExtenderVirtualStorage, ReportWriteProtection)
{ if (!isWriteProtected) return kIOReturnBadArgument; *isWriteProtected = false; return kIOReturnSuccess; }

static bool enqueuePending(ExtenderVirtualStorage_IVars *ivars,
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

kern_return_t IMPL(ExtenderVirtualStorage, DoAsyncEjectMedia)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Eject req=%u", requestID);
    Complete(requestID, kIOReturnSuccess);
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualStorage, DoAsyncSynchronize)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Sync req=%u lba=%llu n=%llu", requestID, lba, numOfBlocks);
    Complete(requestID, kIOReturnSuccess);
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualStorage, DoAsyncReadWrite)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": %s req=%u lba=%llu n=%llu size=%llu",
           isRead ? "Read" : "Write", requestID, lba, numOfBlocks, size);

    // Track this IO so we can complete it when the daemon delivers a response.
    int slot = -1;
    for (uint32_t i = 0; i < kInFlightCapacity; i++) {
        if (!ivars->inFlight[i].valid) { slot = (int)i; break; }
    }
    if (slot < 0) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": in-flight table full");
        CompleteIO(requestID, 0, kIOReturnNoResources);
        return kIOReturnSuccess;
    }
    ivars->inFlight[slot].requestId    = requestID;
    ivars->inFlight[slot].dmaAddr      = dmaAddr;
    ivars->inFlight[slot].expectedSize = size;
    ivars->inFlight[slot].isRead       = isRead;
    ivars->inFlight[slot].valid        = true;

    // For WRITE, copy the data from the DMA buffer into our pending entry so
    // the daemon can read it. For READ, the request payload is just a SCSI hint;
    // the actual data comes back via completeRequest.
    uint8_t scratch[EXTENDER_MAX_TRANSFER_SIZE];
    uint32_t payloadLen = 0;
    if (!isRead && dmaAddr && size > 0) {
        uint32_t copy = size <= EXTENDER_MAX_TRANSFER_SIZE ? (uint32_t)size : EXTENDER_MAX_TRANSFER_SIZE;
        memcpy(scratch, (const void *)(uintptr_t)dmaAddr, copy);
        payloadLen = copy;
    } else {
        // SCSI-style hint for READ: opcode + LBA + count, daemon parses to build the network request.
        scratch[0] = 0x28;
        scratch[1] = (uint8_t)options;
        scratch[2] = (uint8_t)(lba >> 24);
        scratch[3] = (uint8_t)(lba >> 16);
        scratch[4] = (uint8_t)(lba >> 8);
        scratch[5] = (uint8_t)(lba);
        scratch[7] = (uint8_t)(numOfBlocks >> 8);
        scratch[8] = (uint8_t)(numOfBlocks);
        payloadLen = 16;
    }

    PendingOutputEntry &entry = ivars->pending[ivars->pendingTail];
    entry.requestType = isRead ? 0u : 1u;  // 0=bulk-in (READ), 1=bulk-out (WRITE)
    entry.endpoint = 0;
    entry.length = payloadLen;
    entry.requestId = requestID;
    if (payloadLen > 0) memcpy(entry.data, scratch, payloadLen);
    entry.valid = true;
    ivars->pendingTail = (ivars->pendingTail + 1) % kPendingOutputCapacity;
    ivars->pendingCount++;

    return kIOReturnSuccess;
}

void ExtenderVirtualStorage::completeRequest(uint32_t requestId,
                                             kern_return_t status,
                                             const void *bytes,
                                             uint32_t length)
{
    if (!ivars) return;
    for (uint32_t i = 0; i < kInFlightCapacity; i++) {
        InFlightIO &io = ivars->inFlight[i];
        if (!io.valid || io.requestId != requestId) continue;

        uint64_t transferred = 0;
        if (status == kIOReturnSuccess && io.isRead && io.dmaAddr && bytes && length > 0) {
            uint32_t copy = length;
            if ((uint64_t)copy > io.expectedSize) copy = (uint32_t)io.expectedSize;
            memcpy((void *)(uintptr_t)io.dmaAddr, bytes, copy);
            transferred = copy;
        } else if (status == kIOReturnSuccess && !io.isRead) {
            transferred = io.expectedSize;
        }

        CompleteIO(requestId, transferred, status);
        io.valid = false;
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": completed rid=%u status=0x%x bytes=%llu", requestId, status, transferred);
        return;
    }
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": completeRequest: unknown rid=%u", requestId);
}

void ExtenderVirtualStorage::configureGeometry(uint64_t blockCount, uint32_t blockSize)
{
    if (!ivars) return;
    ivars->blockCount = blockCount;
    ivars->blockSize = blockSize > 0 ? blockSize : 512;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": configureGeometry blocks=%llu size=%u", blockCount, blockSize);
}

kern_return_t ExtenderVirtualStorage::DoAsyncUnmapPriv(uint32_t requestID, struct BlockRange *ranges, uint32_t numOfRanges)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Unmap req=%u n=%u", requestID, numOfRanges);
    Complete(requestID, kIOReturnUnsupported);
    return kIOReturnSuccess;
}

void ExtenderVirtualStorage::submitInputReport(const void *data, uint32_t length)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": submitInput len=%u (drop — data path TODO)", length);
    (void)data;
}

bool ExtenderVirtualStorage::dequeuePendingOutput(void *hdrOut, void *bufOut, uint32_t maxLen, uint32_t *outLen)
{
    if (!ivars || !hdrOut || !outLen) return false;
    if (ivars->pendingCount == 0) { *outLen = 0; return false; }
    PendingOutputEntry &entry = ivars->pending[ivars->pendingHead];
    ExtenderPendingOutputHeader *hdr = (ExtenderPendingOutputHeader *)hdrOut;
    hdr->deviceId = 0;
    hdr->endpoint = entry.endpoint;
    hdr->requestType = entry.requestType;
    hdr->requestId = entry.requestId;
    hdr->reserved = 0;
    uint32_t copyLen = entry.length < maxLen ? entry.length : maxLen;
    hdr->dataLength = copyLen;
    if (copyLen > 0 && bufOut) memcpy(bufOut, entry.data, copyLen);
    *outLen = copyLen;
    entry.valid = false;
    ivars->pendingHead = (ivars->pendingHead + 1) % kPendingOutputCapacity;
    ivars->pendingCount--;
    return true;
}
