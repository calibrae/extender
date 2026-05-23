#include <os/log.h>
#include <string.h>
#include <DriverKit/IOLib.h>
#include <DriverKit/IOService.h>
#include <USBDriverKit/IOUSBHostDevice.h>

#include "ExtenderExportDevice.h"
#include "ExtenderProtocol.h"

#define LOG_PREFIX "ExtenderExport"

struct ExtenderExportDevice_IVars {
    IOUSBHostDevice *usbDevice;   // The provider we claimed; weak ref (provider owns).
    bool             opened;      // Whether we hold a session via Open().
    uint32_t         locationId;  // Cached for log/debug.
};

bool ExtenderExportDevice::init()
{
    if (!super::init()) return false;
    ivars = IONewZero(ExtenderExportDevice_IVars, 1);
    if (!ivars) return false;
    ivars->usbDevice = nullptr;
    ivars->opened = false;
    ivars->locationId = 0;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": init");
    return true;
}

kern_return_t IMPL(ExtenderExportDevice, Start)
{
    kern_return_t ret = Start(provider, SUPERDISPATCH);
    if (ret != kIOReturnSuccess) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Start super failed: 0x%x", ret);
        return ret;
    }

    ivars->usbDevice = OSDynamicCast(IOUSBHostDevice, provider);
    if (!ivars->usbDevice) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": provider is not IOUSBHostDevice");
        return kIOReturnBadArgument;
    }

    // Take exclusive ownership of the USB device. After this succeeds, the
    // default class driver (storage/HID/networking kext) cannot bind.
    ret = ivars->usbDevice->Open(this, /*options*/ 0, /*arg*/ 0);
    if (ret != kIOReturnSuccess) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": USB Open failed: 0x%x", ret);
        return ret;
    }
    ivars->opened = true;

    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": claimed USB device (location=0x%x)", ivars->locationId);
    RegisterService();
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderExportDevice, Stop)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Stop");
    if (ivars->opened && ivars->usbDevice) {
        ivars->usbDevice->Close(this, 0);
        ivars->opened = false;
    }
    ivars->usbDevice = nullptr;
    IOSafeDeleteNULL(ivars, ExtenderExportDevice_IVars, 1);
    return Stop(provider, SUPERDISPATCH);
}

kern_return_t ExtenderExportDevice::submitControlTransfer(uint8_t /*requestType*/,
                                                          uint8_t /*request*/,
                                                          uint16_t /*value*/,
                                                          uint16_t /*index*/,
                                                          const void * /*outData*/,
                                                          uint16_t /*outLen*/,
                                                          void * /*inData*/,
                                                          uint16_t * /*inLen*/)
{
    // Real implementation calls ivars->usbDevice->DeviceRequest(this, ...).
    // Scaffold returns Unsupported so callers can detect the stub.
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": submitControlTransfer (stub)");
    return kIOReturnUnsupported;
}

bool ExtenderExportDevice::dequeuePendingResponse(void * /*hdrOut*/,
                                                  void * /*bufOut*/,
                                                  uint32_t /*maxLen*/,
                                                  uint32_t *outLen)
{
    if (outLen) *outLen = 0;
    return false;
}
