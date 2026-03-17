#ifndef ExtenderVirtualStorageDriver_h
#define ExtenderVirtualStorageDriver_h

#include <DriverKit/IOService.h>
#include <DriverKit/IOUserClient.h>
#include <DriverKit/IOBufferMemoryDescriptor.h>

/*!
 * @class ExtenderVirtualStorageDriver
 * @abstract Virtual block storage device for USB/IP remote mass storage.
 *
 * This DriverKit extension presents a virtual SCSI block device to macOS.
 * The host app (Extender daemon) communicates with this driver via
 * IOUserClient, forwarding SCSI read/write commands to/from the remote
 * USB mass storage device over the USB/IP protocol.
 *
 * Architecture:
 *   Remote USB Drive → USB/IP → Extender Daemon → UserClient → This Driver → macOS Disk
 */
class ExtenderVirtualStorageDriver : public IOService
{
    OSDeclareDefaultStructors(ExtenderVirtualStorageDriver)

public:
    /// Called when the driver is loaded.
    virtual kern_return_t Start(IOService *provider) override;

    /// Called when the driver is stopped.
    virtual kern_return_t Stop(IOService *provider) override;

    /// Handle a new user client connection from the Extender daemon.
    virtual kern_return_t NewUserClient(uint32_t type, IOUserClient **userClient) override;

private:
    /// Disk geometry
    uint64_t _blockCount;
    uint32_t _blockSize;

    /// Device identification
    char _vendorID[9];
    char _productID[17];
};

/*!
 * @class ExtenderStorageUserClient
 * @abstract UserClient for communication between Extender daemon and the driver.
 *
 * The daemon sends SCSI block read/write requests via ExternalMethod calls.
 * The driver forwards them to the IOBlockStorageDevice interface.
 */
class ExtenderStorageUserClient : public IOUserClient
{
    OSDeclareDefaultStructors(ExtenderStorageUserClient)

public:
    virtual kern_return_t Start(IOService *provider) override;
    virtual kern_return_t Stop(IOService *provider) override;

    /// Handle method calls from the Extender daemon.
    virtual kern_return_t ExternalMethod(
        uint64_t selector,
        IOUserClientMethodArguments *arguments,
        const IOUserClientMethodDispatch *dispatch,
        OSObject *target,
        void *reference) override;

    /// Method selectors for daemon ↔ driver communication.
    enum MethodSelector : uint64_t {
        kSetDiskGeometry = 0,   // Set block count + block size
        kReadBlocks      = 1,   // Read blocks (driver → daemon callback)
        kWriteBlocks     = 2,   // Write blocks (driver → daemon callback)
        kGetDeviceInfo   = 3,   // Get vendor/product strings
        kNotifyReady     = 4,   // Signal that device is ready for I/O
    };

private:
    ExtenderVirtualStorageDriver *_driver;
};

#endif /* ExtenderVirtualStorageDriver_h */
