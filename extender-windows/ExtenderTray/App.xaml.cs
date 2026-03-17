using System.Diagnostics;
using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using Hardcodet.Wpf.TaskbarNotification;

namespace ExtenderTray;

public partial class App : Application
{
    private TaskbarIcon? _trayIcon;
    private DaemonClient _client = new();
    private Process? _daemonProcess;
    private System.Threading.Timer? _pollTimer;

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);

        _trayIcon = new TaskbarIcon
        {
            ToolTipText = "Extender — USB/IP Device Sharing",
            ContextMenu = BuildContextMenu(),
        };

        // Try connecting to existing daemon, or start one
        _ = InitializeAsync();
    }

    private async Task InitializeAsync()
    {
        if (!await _client.IsRunningAsync())
        {
            StartDaemon();
            await Task.Delay(1500);
        }

        // Start polling for device list
        _pollTimer = new System.Threading.Timer(async _ => await RefreshDevicesAsync(), null, 0, 5000);
    }

    private void StartDaemon()
    {
        var extenderPath = FindExtenderBinary();
        if (extenderPath == null)
        {
            MessageBox.Show("Cannot find extender.exe. Please install Extender first.",
                "Extender", MessageBoxButton.OK, MessageBoxImage.Error);
            return;
        }

        _daemonProcess = new Process
        {
            StartInfo = new ProcessStartInfo
            {
                FileName = extenderPath,
                Arguments = "daemon",
                CreateNoWindow = true,
                UseShellExecute = false,
                WindowStyle = ProcessWindowStyle.Hidden,
            }
        };
        _daemonProcess.Start();
    }

    private ContextMenu BuildContextMenu()
    {
        var menu = new ContextMenu();

        var header = new MenuItem
        {
            Header = "Extender — USB/IP",
            IsEnabled = false,
            FontWeight = FontWeights.Bold,
        };
        menu.Items.Add(header);
        menu.Items.Add(new Separator());

        var statusItem = new MenuItem { Header = "Status: checking...", IsEnabled = false, Tag = "status" };
        menu.Items.Add(statusItem);
        menu.Items.Add(new Separator());

        var devicesHeader = new MenuItem { Header = "USB Devices", IsEnabled = false, Tag = "devices_header" };
        menu.Items.Add(devicesHeader);

        var noDevices = new MenuItem { Header = "  (loading...)", IsEnabled = false, Tag = "no_devices" };
        menu.Items.Add(noDevices);
        menu.Items.Add(new Separator());

        var refreshItem = new MenuItem { Header = "Refresh" };
        refreshItem.Click += async (_, _) => await RefreshDevicesAsync();
        menu.Items.Add(refreshItem);

        var quitItem = new MenuItem { Header = "Quit" };
        quitItem.Click += (_, _) => Shutdown();
        menu.Items.Add(quitItem);

        return menu;
    }

    private async Task RefreshDevicesAsync()
    {
        try
        {
            var devices = await _client.CallAsync("list_local_devices");
            var status = await _client.CallAsync("get_status");

            Dispatcher.Invoke(() => UpdateMenu(devices, status));
        }
        catch
        {
            Dispatcher.Invoke(() =>
            {
                UpdateStatusItem("Status: daemon not running");
                ClearDeviceItems();
            });
        }
    }

    private void UpdateMenu(JsonElement devices, JsonElement status)
    {
        if (_trayIcon?.ContextMenu == null) return;
        var menu = _trayIcon.ContextMenu;

        // Update status
        var uptime = status.GetProperty("uptime_secs").GetUInt64();
        var exported = status.GetProperty("exported_devices").GetUInt32();
        var connected = status.GetProperty("active_connections").GetUInt32();
        UpdateStatusItem($"v{status.GetProperty("version").GetString()} | Up {FormatUptime(uptime)} | {exported} exported, {connected} connected");

        // Update device list
        ClearDeviceItems();

        var insertIndex = FindItemIndex(menu, "devices_header") + 1;

        if (devices.GetArrayLength() == 0)
        {
            var noDevItem = new MenuItem { Header = "  No devices found", IsEnabled = false, Tag = "device" };
            menu.Items.Insert(insertIndex, noDevItem);
            return;
        }

        foreach (var dev in devices.EnumerateArray())
        {
            var busId = dev.GetProperty("bus_id").GetString() ?? "?";
            var vid = dev.GetProperty("vendor_id").GetUInt16();
            var pid = dev.GetProperty("product_id").GetUInt16();
            var product = dev.TryGetProperty("product", out var p) && p.ValueKind == JsonValueKind.String
                ? p.GetString() : null;
            var isBound = dev.TryGetProperty("is_bound", out var b) && b.GetBoolean();
            var speed = dev.TryGetProperty("speed", out var s) ? s.GetString() ?? "" : "";

            var displayName = product ?? $"{vid:x4}:{pid:x4}";
            var label = $"  {busId}  {displayName}  ({speed})" + (isBound ? " [SHARED]" : "");

            var item = new MenuItem { Header = label, Tag = "device" };

            if (isBound)
            {
                var unbind = new MenuItem { Header = "Unbind" };
                var capturedBusId = busId;
                unbind.Click += async (_, _) =>
                {
                    try { await _client.CallAsync("unbind_device", new { bus_id = capturedBusId }); }
                    catch { }
                    await RefreshDevicesAsync();
                };
                item.Items.Add(unbind);
            }
            else
            {
                var bind = new MenuItem { Header = "Bind (share)" };
                var capturedBusId = busId;
                bind.Click += async (_, _) =>
                {
                    try { await _client.CallAsync("bind_device", new { bus_id = capturedBusId }); }
                    catch (Exception ex) { MessageBox.Show(ex.Message, "Bind Failed"); }
                    await RefreshDevicesAsync();
                };
                item.Items.Add(bind);
            }

            menu.Items.Insert(insertIndex++, item);
        }
    }

    private void UpdateStatusItem(string text)
    {
        if (_trayIcon?.ContextMenu == null) return;
        foreach (var item in _trayIcon.ContextMenu.Items.OfType<MenuItem>())
        {
            if (item.Tag as string == "status")
            {
                item.Header = text;
                break;
            }
        }
    }

    private void ClearDeviceItems()
    {
        if (_trayIcon?.ContextMenu == null) return;
        var toRemove = _trayIcon.ContextMenu.Items.OfType<MenuItem>()
            .Where(m => m.Tag as string == "device" || m.Tag as string == "no_devices")
            .ToList();
        foreach (var item in toRemove)
            _trayIcon.ContextMenu.Items.Remove(item);
    }

    private int FindItemIndex(ContextMenu menu, string tag)
    {
        for (int i = 0; i < menu.Items.Count; i++)
        {
            if (menu.Items[i] is MenuItem m && m.Tag as string == tag)
                return i;
        }
        return 0;
    }

    private static string FormatUptime(ulong secs)
    {
        if (secs >= 3600) return $"{secs / 3600}h {(secs % 3600) / 60}m";
        if (secs >= 60) return $"{secs / 60}m {secs % 60}s";
        return $"{secs}s";
    }

    private static string? FindExtenderBinary()
    {
        var candidates = new[]
        {
            Path.Combine(AppContext.BaseDirectory, "extender.exe"),
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
                "extender", "extender", "target", "release", "extender.exe"),
            @"C:\Program Files\Extender\extender.exe",
        };

        foreach (var path in candidates)
            if (File.Exists(path)) return path;

        // Check PATH
        var pathDirs = Environment.GetEnvironmentVariable("PATH")?.Split(';') ?? [];
        foreach (var dir in pathDirs)
        {
            var full = Path.Combine(dir, "extender.exe");
            if (File.Exists(full)) return full;
        }

        return null;
    }

    protected override void OnExit(ExitEventArgs e)
    {
        _pollTimer?.Dispose();
        _trayIcon?.Dispose();
        _daemonProcess?.Kill();
        _daemonProcess?.Dispose();
        base.OnExit(e);
    }
}
