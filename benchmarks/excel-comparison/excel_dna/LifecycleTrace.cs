using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text.Json;

namespace ExcelComparison;

// Observes the managed callback, not Excel-DNA's entire native xlAutoClose export.
// No tracing threads, Excel calls, or changes to the fixture's cleanup behavior.
internal static class LifecycleTrace
{
    private sealed record Sink(string Path, string Token, int Pid, long CreationTicks);
    private static readonly Lazy<Sink?> Output = new(Discover);
    private static long nextInvocation;

    [DllImport("kernel32.dll")]
    private static extern uint GetCurrentThreadId();
    [DllImport("kernel32.dll")]
    private static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetProcessTimes(IntPtr process, out long created, out long exited, out long kernel, out long user);
    [DllImport("kernel32.dll")]
    private static extern void GetSystemTimePreciseAsFileTime(out long value);

    private static Sink? Discover()
    {
        try
        {
            if (!OperatingSystem.IsWindows()) return null;
            int pid = Environment.ProcessId;
            string control = System.IO.Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                "xlfn", "lifecycle-diagnostics", $"{pid}.json");
            if (new FileInfo(control).Length > 16384) return null;
            if (!GetProcessTimes(GetCurrentProcess(), out long created, out _, out _, out _)) return null;
            using var document = JsonDocument.Parse(File.ReadAllText(control));
            var config = document.RootElement;
            if (config.GetProperty("schema").GetInt32() != 1 || config.GetProperty("pid").GetInt32() != pid ||
                config.GetProperty("creation_ticks").GetInt64() != created) return null;
            string path = config.GetProperty("path").GetString()!;
            string token = config.GetProperty("token").GetString()!;
            if (!System.IO.Path.IsPathFullyQualified(path) || token.Length != 32 || !token.All(Uri.IsHexDigit)) return null;
            return new Sink(path, token, pid, created);
        }
        catch { return null; }
    }

    internal static void Write(string name, long? invocation = null)
    {
        try
        {
            if (Output.Value is not Sink sink) return;
            long counter = Stopwatch.GetTimestamp();
            GetSystemTimePreciseAsFileTime(out long now);
            using var memory = new MemoryStream();
            using (var writer = new Utf8JsonWriter(memory))
            {
                writer.WriteStartObject();
                writer.WriteNumber("schema", 1);
                writer.WriteString("source", "excel_dna");
                writer.WriteNumber("pid", sink.Pid);
                writer.WriteNumber("creation_ticks", sink.CreationTicks);
                writer.WriteString("token", sink.Token);
                writer.WriteNumber("qpc_ticks", counter);
                writer.WriteNumber("qpc_frequency", Stopwatch.Frequency);
                writer.WriteNumber("filetime_ticks", now);
                writer.WriteNumber("thread_id", GetCurrentThreadId());
                writer.WriteString("event", name);
                writer.WriteStartObject("details");
                if (invocation.HasValue) writer.WriteNumber("invocation", invocation.Value);
                writer.WriteEndObject();
                writer.WriteEndObject();
            }
            // Append only to the file created by the runner; never truncate it.
            using var file = new FileStream(sink.Path, FileMode.Open, FileAccess.Write, FileShare.ReadWrite);
            file.Seek(0, SeekOrigin.End);
            file.Write(memory.GetBuffer(), 0, (int)memory.Length);
            file.WriteByte((byte)'\n');
        }
        catch { /* Diagnostics must not change the callback result. */ }
    }

    internal sealed class Scope : IDisposable
    {
        private readonly string name;
        private readonly long invocation;
        private bool completed;
        internal Scope(string name)
        {
            this.name = name;
            invocation = Interlocked.Increment(ref nextInvocation);
            Write(name + ".enter", invocation);
        }
        internal void Complete() { Write(name + ".return", invocation); completed = true; }
        public void Dispose() { if (!completed) Write(name + ".scope_exit", invocation); }
    }
}
