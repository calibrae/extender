#include <os/log.h>
#include <string.h>
#include <DriverKit/IOLib.h>

#include "ExtenderVirtualNetwork.h"
#include "ExtenderProtocol.h"

#define LOG_PREFIX "ExtenderNet"
#define EXTENDER_MTU 1500

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

struct ExtenderVirtualNetwork_IVars {
    uint8_t  macAddress[6];
    uint32_t mtu;
    uint32_t hardwareAssists;
    bool     interfaceEnabled;
    bool     promiscuousMode;
    bool     allMulticast;
    bool     wakeOnMagic;

    PendingOutputEntry pending[kPendingOutputCapacity];
    uint32_t           pendingHead;
    uint32_t           pendingTail;
    uint32_t           pendingCount;

    bool started;
};

bool ExtenderVirtualNetwork::init()
{
    if (!super::init()) return false;
    ivars = IONewZero(ExtenderVirtualNetwork_IVars, 1);
    if (!ivars) return false;
    // Locally-administered MAC default: 0x02:00:Ex:te:nd:er-ish placeholder.
    ivars->macAddress[0] = 0x02;
    ivars->macAddress[1] = 0xE0;
    ivars->macAddress[2] = 0x00;
    ivars->macAddress[3] = 0x00;
    ivars->macAddress[4] = 0x00;
    ivars->macAddress[5] = 0x01;
    ivars->mtu = EXTENDER_MTU;
    ivars->hardwareAssists = 0;
    ivars->interfaceEnabled = false;
    ivars->promiscuousMode = false;
    ivars->allMulticast = false;
    ivars->wakeOnMagic = false;
    ivars->started = false;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": init");
    return true;
}

kern_return_t IMPL(ExtenderVirtualNetwork, Start)
{
    kern_return_t ret = Start(provider, SUPERDISPATCH);
    if (ret != kIOReturnSuccess) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Start super failed: 0x%x", ret);
        return ret;
    }
    ivars->started = true;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Started (mac=%02x:%02x:%02x:%02x:%02x:%02x mtu=%u)",
           ivars->macAddress[0], ivars->macAddress[1], ivars->macAddress[2],
           ivars->macAddress[3], ivars->macAddress[4], ivars->macAddress[5], ivars->mtu);
    // NOTE: RegisterEthernetInterface(macAddress, packetPool, queues, queueCount)
    // is the call that actually publishes a network interface to macOS. Skipped
    // for the scaffold — needs IOUserNetworkPacketBufferPool + IOUserNetworkPacketQueue
    // setup which is its own substantial piece of work.
    RegisterService();
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualNetwork, Stop)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Stop");
    ivars->started = false;
    IOSafeDeleteNULL(ivars, ExtenderVirtualNetwork_IVars, 1);
    return Stop(provider, SUPERDISPATCH);
}

kern_return_t IMPL(ExtenderVirtualNetwork, SetInterfaceEnable)
{
    ivars->interfaceEnabled = isEnable;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": SetInterfaceEnable %d", isEnable);
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualNetwork, SetPromiscuousModeEnable)
{
    ivars->promiscuousMode = enable;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": SetPromiscuousModeEnable %d", enable);
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualNetwork, SetMulticastAddresses)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": SetMulticastAddresses count=%u", count);
    (void)addresses;
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualNetwork, SetAllMulticastModeEnable)
{
    ivars->allMulticast = enable;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": SetAllMulticastModeEnable %d", enable);
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualNetwork, SelectMediaType)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": SelectMediaType 0x%llx", (uint64_t)mediaType);
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualNetwork, SetWakeOnMagicPacketEnable)
{
    ivars->wakeOnMagic = enable;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": SetWakeOnMagicPacketEnable %d", enable);
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualNetwork, SetMTU)
{
    ivars->mtu = mtu;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": SetMTU %u", mtu);
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualNetwork, GetMaxTransferUnit)
{
    if (mtu) *mtu = ivars->mtu;
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualNetwork, SetHardwareAssists)
{
    ivars->hardwareAssists = hardwareAssists;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": SetHardwareAssists 0x%x", hardwareAssists);
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualNetwork, GetHardwareAssists)
{
    if (hardwareAssists) *hardwareAssists = ivars->hardwareAssists;
    return kIOReturnSuccess;
}

static bool enqueuePending(ExtenderVirtualNetwork_IVars *ivars,
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

void ExtenderVirtualNetwork::submitInputReport(const void *data, uint32_t length)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": submitInputReport len=%u (drop — packet queue TODO)", length);
    (void)data;
}

bool ExtenderVirtualNetwork::dequeuePendingOutput(void *hdrOut, void *bufOut, uint32_t maxLen, uint32_t *outLen)
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
