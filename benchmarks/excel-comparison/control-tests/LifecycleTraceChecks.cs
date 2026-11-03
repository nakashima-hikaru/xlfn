using System.Diagnostics;
using System.Text.Json;
using ExcelComparison;

internal static class LifecycleTraceChecks
{
    internal static void Run()
    {
        if (!OperatingSystem.IsWindows()) return;
        string directory = Path.Combine(Path.GetTempPath(), "lifecycle-診断-" + Guid.NewGuid());
        Directory.CreateDirectory(directory);
        string output = Path.Combine(directory, "events.jsonl");
        File.WriteAllText(output, "");
        string control = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "xlfn", "lifecycle-diagnostics", $"{Environment.ProcessId}.json");
        Directory.CreateDirectory(Path.GetDirectoryName(control)!);
        byte[]? previous = File.Exists(control) ? File.ReadAllBytes(control) : null;
        string token = Guid.NewGuid().ToString("N");
        long created = Process.GetCurrentProcess().StartTime.ToUniversalTime().ToFileTimeUtc();
        try
        {
            File.WriteAllText(control, JsonSerializer.Serialize(new {
                schema = 1, pid = Environment.ProcessId, creation_ticks = created, token, path = output }));
            using (var trace = new LifecycleTrace.Scope("IExcelAddIn.AutoClose")) trace.Complete();
            using (var trace = new LifecycleTrace.Scope("incomplete")) { }
            string[] expected = { "IExcelAddIn.AutoClose.enter", "IExcelAddIn.AutoClose.return", "incomplete.enter", "incomplete.scope_exit" };
            string[] lines = File.ReadAllLines(output);
            if (lines.Length != expected.Length) throw new Exception("Lifecycle trace missing events");
            for (int i = 0; i < lines.Length; i++)
            {
                using var document = JsonDocument.Parse(lines[i]);
                var entry = document.RootElement;
                if (entry.GetProperty("event").GetString() != expected[i] ||
                    entry.GetProperty("token").GetString() != token ||
                    entry.GetProperty("pid").GetInt32() != Environment.ProcessId ||
                    entry.GetProperty("creation_ticks").GetInt64() != created ||
                    entry.GetProperty("qpc_frequency").GetInt64() != Stopwatch.Frequency ||
                    entry.GetProperty("thread_id").GetUInt32() == 0)
                    throw new Exception("Lifecycle trace identity, clock or boundary mismatch");
            }
            Console.WriteLine("Lifecycle trace native checks passed");
        }
        finally
        {
            if (previous is null) File.Delete(control); else File.WriteAllBytes(control, previous);
            Directory.Delete(directory, recursive: true);
        }
    }
}
