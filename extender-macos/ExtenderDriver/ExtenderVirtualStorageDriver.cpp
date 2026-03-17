#include "ExtenderVirtualStorageDriver.h"

#include <os/log.h>
#include <DriverKit/IOLib.h>

#define LOG_PREFIX "ExtenderDriver"

// MARK: - ExtenderVirtualStorageDriver

OSDefineMetaClassAndStructors(ExtenderVirtualStorageDriver, IOService)

kern_return_t ExtenderVirtualStorageDriver::Start(IOService *provider)
{
    kern_return_t ret = IOService::Start(provider);
    if (ret != kIOReturnSuccess) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Start failed: %d", ret);
        return ret;
    }

    // Initialize with default values; real geometry set by UserClient.
    _blockCount = 0;
    _blockSize = 512;
    memset(_vendorID, 0, sizeof(_vendorID));
    memset(_productID, 0, sizeof(_productID));
    strncpy(_vendorID, "Extender", 8);
    strncpy(_productID, "Virtual Disk", 16);

    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Driver started, waiting for daemon connection");

    // Register the service so the daemon can find us.
    RegisterService();

    return kIOReturnSuccess;
}

kern_return_t ExtenderVirtualStorageDriver::Stop(IOService *provider)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Driver stopped");
    return IOService::Stop(provider);
}

kern_return_t ExtenderVirtualStorageDriver::NewUserClient(uint32_t type, IOUserClient **userClient)
{
    IOService *client = nullptr;

    auto ret = Create(this, "UserClientProperties", &client);
    if (ret != kIOReturnSuccess) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Failed to create UserClient: %d", ret);
        return ret;
    }

    *userClient = OSDynamicCast(IOUserClient, client);
    if (*userClient == nullptr) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Failed to cast to IOUserClient");
        client->release();
        return kIOReturnError;
    }

    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": New UserClient connected");
    return kIOReturnSuccess;
}

// MARK: - ExtenderStorageUserClient

OSDefineMetaClassAndStructors(ExtenderStorageUserClient, IOUserClient)

kern_return_t ExtenderStorageUserClient::Start(IOService *provider)
{
    kern_return_t ret = IOUserClient::Start(provider);
    if (ret != kIOReturnSuccess) {
        return ret;
    }

    _driver = OSDynamicCast(ExtenderVirtualStorageDriver, provider);
    if (_driver == nullptr) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": UserClient provider is not ExtenderVirtualStorageDriver");
        return kIOReturnError;
    }

    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": UserClient started");
    return kIOReturnSuccess;
}

kern_return_t ExtenderStorageUserClient::Stop(IOService *provider)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": UserClient stopped");
    _driver = nullptr;
    return IOUserClient::Stop(provider);
}

kern_return_t ExtenderStorageUserClient::ExternalMethod(
    uint64_t selector,
    IOUserClientMethodArguments *arguments,
    const IOUserClientMethodDispatch *dispatch,
    OSObject *target,
    void *reference)
{
    switch (static_cast<MethodSelector>(selector)) {
    case kSetDiskGeometry:
        // arguments->scalarInput[0] = blockCount
        // arguments->scalarInput[1] = blockSize
        if (arguments->scalarInputCount < 2) {
            return kIOReturnBadArgument;
        }
        if (_driver) {
            _driver->_blockCount = arguments->scalarInput[0];
            _driver->_blockSize = static_cast<uint32_t>(arguments->scalarInput[1]);
            os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Geometry set: %llu blocks x %u bytes",
                   _driver->_blockCount, _driver->_blockSize);
        }
        return kIOReturnSuccess;

    case kNotifyReady:
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Device ready for I/O");
        // TODO: Publish IOBlockStorageDevice nub for macOS to discover
        return kIOReturnSuccess;

    case kGetDeviceInfo:
        // Return vendor/product as scalar outputs
        if (arguments->scalarOutputCount < 2 || _driver == nullptr) {
            return kIOReturnBadArgument;
        }
        // Pack first 8 chars of vendor/product into uint64s
        memcpy(&arguments->scalarOutput[0], _driver->_vendorID, 8);
        memcpy(&arguments->scalarOutput[1], _driver->_productID, 8);
        return kIOReturnSuccess;

    case kReadBlocks:
    case kWriteBlocks:
        // TODO: These will be implemented as async callbacks.
        // The driver receives a read/write request from macOS,
        // forwards it to the daemon via UserClient callback,
        // daemon sends SCSI commands over USB/IP, returns data.
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Block I/O not yet implemented");
        return kIOReturnUnsupported;

    default:
        return kIOReturnBadArgument;
    }
}
