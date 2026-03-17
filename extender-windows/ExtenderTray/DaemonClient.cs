using System.Net.Sockets;
using System.Text;
using System.Text.Json;

namespace ExtenderTray;

/// <summary>
/// JSON-RPC 2.0 client that talks to the Extender daemon over TCP localhost.
/// Messages are length-prefixed: 4-byte big-endian length + JSON payload.
/// </summary>
public class DaemonClient
{
    private readonly string _host;
    private readonly int _port;
    private int _nextId = 1;

    public DaemonClient(string host = "127.0.0.1", int port = 9241)
    {
        _host = host;
        _port = port;
    }

    public async Task<JsonElement> CallAsync(string method, object? parameters = null)
    {
        var request = new
        {
            jsonrpc = "2.0",
            method,
            @params = parameters,
            id = _nextId++
        };

        var requestJson = JsonSerializer.Serialize(request);
        var requestBytes = Encoding.UTF8.GetBytes(requestJson);

        using var client = new TcpClient();
        await client.ConnectAsync(_host, _port);
        using var stream = client.GetStream();

        // Write: 4-byte big-endian length + payload
        var lengthBytes = BitConverter.GetBytes((uint)requestBytes.Length);
        if (BitConverter.IsLittleEndian) Array.Reverse(lengthBytes);
        await stream.WriteAsync(lengthBytes);
        await stream.WriteAsync(requestBytes);

        // Read: 4-byte big-endian length
        var responseLengthBytes = new byte[4];
        await ReadExactAsync(stream, responseLengthBytes);
        if (BitConverter.IsLittleEndian) Array.Reverse(responseLengthBytes);
        var responseLength = BitConverter.ToUInt32(responseLengthBytes);

        // Read: payload
        var responseBytes = new byte[responseLength];
        await ReadExactAsync(stream, responseBytes);

        var responseJson = Encoding.UTF8.GetString(responseBytes);
        var response = JsonSerializer.Deserialize<JsonElement>(responseJson);

        if (response.TryGetProperty("error", out var error) && error.ValueKind != JsonValueKind.Null)
        {
            var message = error.GetProperty("message").GetString() ?? "Unknown error";
            throw new Exception($"Daemon error: {message}");
        }

        if (response.TryGetProperty("result", out var result))
            return result;

        return default;
    }

    public async Task<bool> IsRunningAsync()
    {
        try
        {
            await CallAsync("get_status");
            return true;
        }
        catch
        {
            return false;
        }
    }

    private static async Task ReadExactAsync(NetworkStream stream, byte[] buffer)
    {
        int offset = 0;
        while (offset < buffer.Length)
        {
            int read = await stream.ReadAsync(buffer.AsMemory(offset, buffer.Length - offset));
            if (read == 0) throw new Exception("Connection closed");
            offset += read;
        }
    }
}
