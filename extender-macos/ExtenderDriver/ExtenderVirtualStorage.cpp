#include <os/log.h>
#include <DriverKit/IOLib.h>
#include <DriverKit/IOService.h>

#include "ExtenderVirtualStorage.h"
#include "ExtenderProtocol.h"

#define LOG_PREFIX "ExtenderStorage"

struct ExtenderVirtualStorage_IVars {
    // Geometry
    uint64_t blockCount;
    uint32_t blockSize;

    // Device identity
    uint16_t vendorId;
    uint16_t productId;
    char     productName[64];

    // State
    bool     started;
    uint32_t deviceId;

    // Pending I/O queue
    struct PendingIO {
        uint64_t offset;       // Block offset
        uint32_t length;       // Number of blocks
        bool     isWrite;      // true = write, false = read
        uint8_t  data[EXTENDER_MAX_TRANSFER_SIZE];
        uint32_t dataLength;
        bool     valid;
    };

    static const uint32_t kMaxPendingIO = 16;
    PendingIO pendingIO[16];
    uint32_t  pendingHead;
    uint32_t  pendingTail;
    uint32_t  pendingCount;
};

bool ExtenderVirtualStorage::init()
{
    if (!super::init()) return false;

    ivars = IONewZero(ExtenderVirtualStorage_IVars, 1);
    if (!ivars) return false;

    ivars->blockCount = 0;
    ivars->blockSize = 512;
    ivars->vendorId = 0;
    ivars->productId = 0;
    strncpy(ivars->productName, "Extender Virtual Disk", sizeof(ivars->productName) - 1);
    ivars->started = false;
    ivars->deviceId = 0;
    ivars->pendingHead = 0;
    ivars->pendingTail = 0;
    ivars->pendingCount = 0;

    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": init");
    return true;
}

kern_return_t IMPL(ExtenderVirtualStorage, Start)
{
    kern_return_t ret = Start(provider, SUPERDISPATCH);
    if (ret != kIOReturnSuccess) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Start failed: 0x%x", ret);
        return ret;
    }

    ivars->started = true;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Device started (%llu blocks x %u bytes = %llu MB)",
           ivars->blockCount, ivars->blockSize,
           (ivars->blockCount * ivars->blockSize) / (1024 * 1024));

    RegisterService();
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualStorage, Stop)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Device stopped");
    ivars->started = false;

    IOSafeDeleteNULL(ivars, ExtenderVirtualStorage_IVars, 1);

    return Stop(provider, SUPERDISPATCH);
}
