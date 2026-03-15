# USB/IP Protocol Specification (from Linux Kernel docs)

## Architecture Overview

USB/IP implements a server/client model where servers export USB devices and clients import them. The client machine runs the device driver for imported devices. Communication occurs via TCP/IP connections (default port 3240) using standardized packet formats.

## Protocol Version

USBIP version v1.1.1, represented as 0x0111 in binary message headers.

## Byte Order

All fields use network (big-endian) byte order.

---

## Device Discovery & Import Messages

### OP_REQ_DEVLIST (0x8005)

Requests the list of exported USB devices.

| Offset | Length | Value         | Purpose            |
|--------|--------|---------------|--------------------|
| 0      | 2      | USBIP version | Protocol version   |
| 2      | 2      | 0x8005        | Command identifier |
| 4      | 4      | 0x00000000    | Status (unused)    |

### OP_REP_DEVLIST (0x0005)

Returns available exported devices with full device descriptors.

| Offset | Length  | Purpose                                                        |
|--------|---------|----------------------------------------------------------------|
| 0      | 2       | USBIP version                                                 |
| 2      | 2       | 0x0005 reply code                                              |
| 4      | 4       | Status (0 = success)                                           |
| 8      | 4       | Number of exported devices                                     |
| 0x0C   | 256     | Device path string                                             |
| 0x10C  | 32      | Bus ID string                                                  |
| 0x12C+ | Variable| Device descriptors (vendor ID, product ID, class, interfaces)  |

### OP_REQ_IMPORT (0x8003)

Requests attachment of a remote USB device.

| Offset | Length | Purpose           |
|--------|--------|-------------------|
| 0      | 2      | USBIP version     |
| 2      | 2      | 0x8003 command code|
| 4      | 4      | Status (unused)    |
| 8      | 32     | Bus ID of target   |

### OP_REP_IMPORT (0x0003)

Confirms device import with status and device details if successful.

| Offset | Length   | Purpose                           |
|--------|----------|-----------------------------------|
| 0      | 2       | USBIP version                     |
| 2      | 2       | 0x0003 reply code                  |
| 4      | 4       | Status (0 = success, 1 = error)    |
| 8+     | Variable| Device details (if status = 0)     |

---

## URB Transfer Messages

### Common Header: usbip_header_basic (20 bytes)

All URB-related commands share this foundation:

| Offset | Length | Field            |
|--------|--------|------------------|
| 0      | 4      | Command type     |
| 4      | 4      | Sequential number|
| 8      | 4      | Device ID        |
| 0xC    | 4      | Direction (0=OUT, 1=IN) |
| 0x10   | 4      | Endpoint number  |

### USBIP_CMD_SUBMIT (0x00000001)

Submits a USB request (URB).

| Offset | Length | Field                       |
|--------|--------|-----------------------------|
| 0      | 20     | usbip_header_basic          |
| 0x14   | 4      | Transfer flags              |
| 0x18   | 4      | Buffer length               |
| 0x1C   | 4      | Start frame (ISO transfers) |
| 0x20   | 4      | Number of packets           |
| 0x24   | 4      | Interval                    |
| 0x28   | 8      | USB setup data              |
| 0x30   | n      | Transfer buffer payload     |
| 0x30+n | m      | ISO packet descriptors      |

### USBIP_RET_SUBMIT (0x00000003)

Returns URB submission results.

| Offset | Length | Field              |
|--------|--------|--------------------|
| 0      | 20     | usbip_header_basic |
| 0x14   | 4      | Status (0=success) |
| 0x18   | 4      | Actual data length |
| 0x1C   | 4      | Start frame        |
| 0x20   | 4      | Number of packets  |
| 0x24   | 4      | Error count        |
| 0x28   | 8      | Padding            |
| 0x30   | n      | Transfer buffer    |
| 0x30+n | m      | ISO descriptors    |

### USBIP_CMD_UNLINK (0x00000002)

Cancels a previously submitted URB.

| Offset | Length | Field                       |
|--------|--------|-----------------------------|
| 0      | 20     | usbip_header_basic          |
| 0x14   | 4      | Sequence number to unlink   |
| 0x18   | 24     | Padding                     |

### USBIP_RET_UNLINK (0x00000004)

Confirms URB cancellation.

| Offset | Length | Field                                 |
|--------|--------|---------------------------------------|
| 0      | 20     | usbip_header_basic                    |
| 0x14   | 4      | Status (-ECONNRESET if successful)    |
| 0x18   | 24     | Padding                               |

---

## Protocol Flow

1. Client opens TCP connection and sends `OP_REQ_DEVLIST`
2. Server responds with `OP_REP_DEVLIST` listing available devices
3. Connection closes
4. Client opens new TCP connection and sends `OP_REQ_IMPORT` with desired bus ID
5. Server responds with `OP_REP_IMPORT`; connection remains open for URB traffic
6. Client sends `USBIP_CMD_SUBMIT` packets with sequential numbers
7. Server responds with `USBIP_RET_SUBMIT` for each submission
8. Client may send `USBIP_CMD_UNLINK` to cancel requests
9. Server responds with `USBIP_RET_UNLINK`; cancelled URBs receive no return submission

---

## Kernel Modules

- **Server side:** `usbip_host` (physical devices), `usbip_vudc` (virtual USB Device Controller for gadgets)
- **Client side:** `vhci_hcd` (Virtual Host Controller Interface)

## Sources

- https://docs.kernel.org/usb/usbip_protocol.html
- https://wiki.archlinux.org/title/USB/IP
- https://github.com/torvalds/linux/tree/master/tools/usb/usbip
