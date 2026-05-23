#include <os/log.h>
#include <string.h>
#include <DriverKit/IOLib.h>
#include <DriverKit/IOService.h>

#include "ExtenderVirtualSerial.h"
#include "ExtenderProtocol.h"

#define LOG_PREFIX "ExtenderSerial"

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

struct ExtenderVirtualSerial_IVars {
    uint32_t baudRate;
    uint8_t  dataBits;
    uint8_t  halfStopBits;
    uint8_t  parity;
    bool     dtr;
    bool     rts;

    PendingOutputEntry pending[kPendingOutputCapacity];
    uint32_t           pendingHead;
    uint32_t           pendingTail;
    uint32_t           pendingCount;

    bool     started;
};

bool ExtenderVirtualSerial::init()
{
    if (!super::init()) return false;
    ivars = IONewZero(ExtenderVirtualSerial_IVars, 1);
    if (!ivars) return false;
    ivars->baudRate = 9600;
    ivars->dataBits = 8;
    ivars->halfStopBits = 2;
    ivars->parity = 0;
    ivars->dtr = false;
    ivars->rts = false;
    ivars->started = false;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": init");
    return true;
}

kern_return_t IMPL(ExtenderVirtualSerial, Start)
{
    kern_return_t ret = Start(provider, SUPERDISPATCH);
    if (ret != kIOReturnSuccess) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Start super failed: 0x%x", ret);
        return ret;
    }
    ivars->started = true;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Started");
    RegisterService();
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualSerial, Stop)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Stop");
    ivars->started = false;
    IOSafeDeleteNULL(ivars, ExtenderVirtualSerial_IVars, 1);
    return Stop(provider, SUPERDISPATCH);
}

kern_return_t IMPL(ExtenderVirtualSerial, HwResetFIFO)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": HwResetFIFO tx=%d rx=%d", tx, rx);
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualSerial, HwSendBreak)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": HwSendBreak %d", sendBreak);
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualSerial, HwProgramUART)
{
    ivars->baudRate = baudRate;
    ivars->dataBits = nDataBits;
    ivars->halfStopBits = nHalfStopBits;
    ivars->parity = parity;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": HwProgramUART baud=%u data=%u stop/2=%u parity=%u",
           baudRate, nDataBits, nHalfStopBits, parity);
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualSerial, HwProgramBaudRate)
{
    ivars->baudRate = baudRate;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": HwProgramBaudRate %u", baudRate);
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualSerial, HwProgramMCR)
{
    ivars->dtr = dtr;
    ivars->rts = rts;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": HwProgramMCR dtr=%d rts=%d", dtr, rts);
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualSerial, HwGetModemStatus)
{
    if (cts) *cts = true;
    if (dsr) *dsr = true;
    if (ri)  *ri  = false;
    if (dcd) *dcd = true;
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualSerial, HwProgramLatencyTimer)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": HwProgramLatencyTimer %u", latency);
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualSerial, HwProgramFlowControl)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": HwProgramFlowControl arg=%u xon=0x%02x xoff=0x%02x", arg, xon, xoff);
    return kIOReturnSuccess;
}

static bool enqueuePending(ExtenderVirtualSerial_IVars *ivars,
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

void ExtenderVirtualSerial::submitInputReport(const void *data, uint32_t length)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": submitInputReport len=%u (drop — rx queue TODO)", length);
    (void)data;
}

bool ExtenderVirtualSerial::dequeuePendingOutput(void *hdrOut, void *bufOut, uint32_t maxLen, uint32_t *outLen)
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
