import Foundation

/// JSON-RPC 2.0 request.
struct JSONRPCRequest: Codable {
    let jsonrpc: String
    let method: String
    let params: [String: AnyCodable]?
    let id: Int

    init(method: String, params: [String: Any]? = nil, id: Int) {
        self.jsonrpc = "2.0"
        self.method = method
        self.params = params?.mapValues { AnyCodable($0) }
        self.id = id
    }
}

/// JSON-RPC 2.0 response.
struct JSONRPCResponse: Codable {
    let jsonrpc: String?
    let result: AnyCodable?
    let error: JSONRPCError?
    let id: Int?
}

/// JSON-RPC 2.0 error.
struct JSONRPCError: Codable {
    let code: Int
    let message: String
    let data: AnyCodable?
}

/// Type-erased Codable wrapper for heterogeneous JSON values.
struct AnyCodable: Codable {
    let value: Any

    init(_ value: Any) {
        self.value = value
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            value = NSNull()
        } else if let bool = try? container.decode(Bool.self) {
            value = bool
        } else if let int = try? container.decode(Int.self) {
            value = int
        } else if let double = try? container.decode(Double.self) {
            value = double
        } else if let string = try? container.decode(String.self) {
            value = string
        } else if let array = try? container.decode([AnyCodable].self) {
            value = array.map { $0.value }
        } else if let dict = try? container.decode([String: AnyCodable].self) {
            value = dict.mapValues { $0.value }
        } else {
            throw DecodingError.dataCorruptedError(in: container, debugDescription: "Unsupported type")
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch value {
        case is NSNull:
            try container.encodeNil()
        case let bool as Bool:
            try container.encode(bool)
        case let int as Int:
            try container.encode(int)
        case let double as Double:
            try container.encode(double)
        case let string as String:
            try container.encode(string)
        case let array as [Any]:
            try container.encode(array.map { AnyCodable($0) })
        case let dict as [String: Any]:
            try container.encode(dict.mapValues { AnyCodable($0) })
        default:
            try container.encodeNil()
        }
    }
}

/// Client for communicating with the Extender daemon over a Unix socket using JSON-RPC 2.0.
///
/// Messages are length-prefixed: 4-byte big-endian length followed by JSON payload.
public final class DaemonClient {
    private let socketPath: String
    private var nextId = 1

    public init(socketPath: String? = nil) {
        self.socketPath = socketPath ?? DaemonClient.defaultSocketPath()
    }

    /// Determine the default socket path (mirrors the Rust daemon logic).
    static func defaultSocketPath() -> String {
        if getuid() == 0 {
            return "/var/run/extender.sock"
        }
        if let xdg = ProcessInfo.processInfo.environment["XDG_RUNTIME_DIR"] {
            return "\(xdg)/extender.sock"
        }
        return "/tmp/extender.sock"
    }

    /// Call a JSON-RPC method and return the parsed result.
    public func call<T: Decodable>(method: String, params: [String: Any]? = nil) async throws -> T {
        let data = try await callRaw(method: method, params: params)
        return try JSONDecoder().decode(T.self, from: data)
    }

    /// Call a JSON-RPC method and return the raw JSON result data.
    public func callRaw(method: String, params: [String: Any]? = nil) async throws -> Data {
        let id = nextId
        nextId += 1

        let request = JSONRPCRequest(method: method, params: params, id: id)
        let requestData = try JSONEncoder().encode(request)

        let responseData = try await sendReceive(requestData)

        let response = try JSONDecoder().decode(JSONRPCResponse.self, from: responseData)

        if let error = response.error {
            throw DaemonError.rpcError(code: error.code, message: error.message)
        }

        guard let result = response.result else {
            throw DaemonError.noResult
        }

        // Re-encode the result for the caller to decode into their target type
        return try JSONEncoder().encode(result)
    }

    /// Low-level send/receive over the Unix socket with length-prefixed framing.
    private func sendReceive(_ data: Data) async throws -> Data {
        let addr = sockaddr_un.make(path: socketPath)

        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else {
            throw DaemonError.connectionFailed(reason: "socket() failed: \(String(cString: strerror(errno)))")
        }
        defer { close(fd) }

        let connected = addr.withUnsafePointer { ptr, len in
            Darwin.connect(fd, ptr, len)
        }
        guard connected == 0 else {
            throw DaemonError.connectionFailed(reason: "connect() to \(socketPath) failed: \(String(cString: strerror(errno)))")
        }

        // Write: 4-byte big-endian length + payload
        var length = UInt32(data.count).bigEndian
        let lengthData = Data(bytes: &length, count: 4)
        try writeAll(fd: fd, data: lengthData)
        try writeAll(fd: fd, data: data)

        // Read: 4-byte big-endian length
        let responseLengthData = try readExact(fd: fd, count: 4)
        let responseLength = responseLengthData.withUnsafeBytes { $0.load(as: UInt32.self).bigEndian }

        guard responseLength > 0 && responseLength < 65536 else {
            throw DaemonError.invalidResponse(reason: "response length \(responseLength) out of range")
        }

        // Read: payload
        return try readExact(fd: fd, count: Int(responseLength))
    }

    private func writeAll(fd: Int32, data: Data) throws {
        var offset = 0
        while offset < data.count {
            let written = data.withUnsafeBytes { ptr in
                write(fd, ptr.baseAddress!.advanced(by: offset), data.count - offset)
            }
            guard written > 0 else {
                throw DaemonError.connectionFailed(reason: "write failed: \(String(cString: strerror(errno)))")
            }
            offset += written
        }
    }

    private func readExact(fd: Int32, count: Int) throws -> Data {
        var buffer = Data(count: count)
        var offset = 0
        while offset < count {
            let bytesRead = buffer.withUnsafeMutableBytes { ptr in
                read(fd, ptr.baseAddress!.advanced(by: offset), count - offset)
            }
            guard bytesRead > 0 else {
                throw DaemonError.connectionFailed(reason: "read failed: connection closed")
            }
            offset += bytesRead
        }
        return buffer
    }
}

/// Errors from the daemon client.
public enum DaemonError: LocalizedError {
    case connectionFailed(reason: String)
    case rpcError(code: Int, message: String)
    case noResult
    case invalidResponse(reason: String)

    public var errorDescription: String? {
        switch self {
        case .connectionFailed(let reason): return "Cannot connect to daemon: \(reason)"
        case .rpcError(_, let message): return "Daemon error: \(message)"
        case .noResult: return "No result from daemon"
        case .invalidResponse(let reason): return "Invalid response: \(reason)"
        }
    }
}

// MARK: - sockaddr_un helper

private extension sockaddr_un {
    static func make(path: String) -> Self {
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = path.utf8CString
        let maxLen = MemoryLayout.size(ofValue: addr.sun_path)
        withUnsafeMutableBytes(of: &addr.sun_path) { rawPtr in
            let count = min(pathBytes.count, maxLen)
            for i in 0..<count {
                rawPtr[i] = UInt8(bitPattern: pathBytes[i])
            }
        }
        return addr
    }

    func withUnsafePointer<T>(_ body: (UnsafePointer<sockaddr>, socklen_t) -> T) -> T {
        var copy = self
        let len = socklen_t(MemoryLayout<sockaddr_un>.size)
        return Swift.withUnsafePointer(to: &copy) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sockPtr in
                body(sockPtr, len)
            }
        }
    }
}
