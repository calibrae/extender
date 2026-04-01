#include <os/log.h>
#include <DriverKit/IOLib.h>
#include <DriverKit/IOService.h>

#include "ExtenderVirtualSerial.h"
#include "ExtenderProtocol.h"

#define LOG_PREFIX "ExtenderSerial"

// Ring buffer size for serial data
#define EXTENDER_SERIAL_BUFFER_SIZE 16384

struct ExtenderVirtualSerial_IVars {
    // Line coding
    uint32_t baudRate;
    uint8_t  dataBits;
    uint8_t  parity;
    uint8_t  stopBits;

    // Device identity
    uint16_t vendorId;
    uint16_t productId;
    char     productName[64];

    // State
    bool     started;
    uint32_t deviceId;
    bool     dtrState;
    bool     rtsState;

    // Data buffers (ring buffers)
    // RX: data from remote device -> macOS applications
    uint8_t  rxBuffer[EXTENDER_SERIAL_BUFFER_SIZE];
    uint32_t rxHead;
    uint32_t rxTail;
    uint32_t rxCount;

    // TX: data from macOS applications -> remote device
    uint8_t  txBuffer[EXTENDER_SERIAL_BUFFER_SIZE];
    uint32_t txHead;
    uint32_t txTail;
    uint32_t txCount;
};

bool ExtenderVirtualSerial::init()
{
    if (!super::init()) return false;

    ivars = IONewZero(ExtenderVirtualSerial_IVars, 1);
    if (!ivars) return false;

    // Default: 9600 8N1
    ivars->baudRate = 9600;
    ivars->dataBits = 8;
    ivars->parity = 0;
    ivars->stopBits = 1;

    ivars->vendorId = 0;
    ivars->productId = 0;
    strncpy(ivars->productName, "Extender Virtual Serial", sizeof(ivars->productName) - 1);
    ivars->started = false;
    ivars->deviceId = 0;
    ivars->dtrState = false;
    ivars->rtsState = false;

    ivars->rxHead = ivars->rxTail = ivars->rxCount = 0;
    ivars->txHead = ivars->txTail = ivars->txCount = 0;

    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": init");
    return true;
}

kern_return_t IMPL(ExtenderVirtualSerial, Start)
{
    kern_return_t ret = Start(provider, SUPERDISPATCH);
    if (ret != kIOReturnSuccess) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Start failed: 0x%x", ret);
        return ret;
    }

    ivars->started = true;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Device started (%u baud, %u%c%u)",
           ivars->baudRate, ivars->dataBits,
           ivars->parity == 0 ? 'N' : (ivars->parity == 1 ? 'O' : 'E'),
           ivars->stopBits);

    RegisterService();
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualSerial, Stop)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Device stopped");
    ivars->started = false;

    IOSafeDeleteNULL(ivars, ExtenderVirtualSerial_IVars, 1);

    return Stop(provider, SUPERDISPATCH);
}
