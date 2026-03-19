#include <os/log.h>
#include <DriverKit/IOLib.h>
#include <DriverKit/IOUserClient.h>

#include "ExtenderStorageUserClient.h"
#include "ExtenderVirtualStorageDriver.h"

#define LOG_PREFIX "ExtenderUC"

struct ExtenderStorageUserClient_IVars {
    ExtenderVirtualStorageDriver *driver;
};

bool ExtenderStorageUserClient::init()
{
    if (!super::init()) return false;

    ivars = IONewZero(ExtenderStorageUserClient_IVars, 1);
    if (!ivars) return false;

    return true;
}

kern_return_t IMPL(ExtenderStorageUserClient, Start)
{
    kern_return_t ret = Start(provider, SUPERDISPATCH);
    if (ret != kIOReturnSuccess) return ret;

    ivars->driver = OSDynamicCast(ExtenderVirtualStorageDriver, provider);
    if (!ivars->driver) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": wrong provider type");
        return kIOReturnError;
    }

    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": UserClient started");
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderStorageUserClient, Stop)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": UserClient stopped");
    ivars->driver = nullptr;

    if (ivars) {
        IOSafeDeleteNULL(ivars, ExtenderStorageUserClient_IVars, 1);
    }

    return Stop(provider, SUPERDISPATCH);
}

kern_return_t ExtenderStorageUserClient::ExternalMethod(
    uint64_t selector,
    IOUserClientMethodArguments *arguments,
    const IOUserClientMethodDispatch *dispatch,
    OSObject *target,
    void *reference)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": ExternalMethod selector=%llu", selector);

    switch (selector) {
    case 0: // kSetDiskGeometry
        if (arguments && arguments->scalarInputCount >= 2) {
            os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Geometry: %llu x %llu",
                   arguments->scalarInput[0], arguments->scalarInput[1]);
        }
        return kIOReturnSuccess;

    case 4: // kNotifyReady
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Device ready");
        return kIOReturnSuccess;

    case 3: // kGetDeviceInfo
        return kIOReturnSuccess;

    case 1: // kReadBlocks
    case 2: // kWriteBlocks
        return kIOReturnUnsupported;

    default:
        return kIOReturnBadArgument;
    }
}
