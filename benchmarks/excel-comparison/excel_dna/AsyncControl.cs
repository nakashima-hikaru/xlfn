using System.Globalization;

namespace ExcelComparison;

// This observer never calls Excel. A calculation call may itself wait for
// native async completion, so a COM callback cannot safely release its gate.
internal static class AsyncGateControl
{
    private static readonly object Gate = new();
    private static TaskCompletionSource<bool> _release = NewRelease();
    private static Thread? _worker;
    private static long _active, _finished, _started, _dropped;
    private static int _armed, _released, _stop;

    private static TaskCompletionSource<bool> NewRelease()
        => new(TaskCreationOptions.RunContinuationsAsynchronously);

    public static long Active => Interlocked.Read(ref _active);
    public static long Finished => Interlocked.Read(ref _finished);
    public static Task WaitAsync() => _release.Task;

    public static void Finish(bool completed)
    {
        if (Volatile.Read(ref _armed) != 0 && !completed)
            Interlocked.Increment(ref _dropped);
        Interlocked.Increment(ref _finished);
        Interlocked.Decrement(ref _active);
    }

    public static void Enter()
    {
        if (Volatile.Read(ref _armed) != 0) Interlocked.Increment(ref _started);
        Interlocked.Increment(ref _active);
    }

    public static void Release()
    {
        Volatile.Write(ref _released, 1);
        _release.TrySetResult(true);
    }

    public static void Arm(string directory, int expected)
    {
        if (expected < 1 || expected > 4096 || !Directory.Exists(directory))
            throw new ArgumentException("Invalid gate directory or expected count");
        lock (Gate)
        {
            if (_worker is not null || Active != 0)
                throw new InvalidOperationException("Gate is already armed or has active tasks");
            foreach (string name in new[] { "ready.json", "release", "released.json", "state.json", "control-error.txt" })
                if (File.Exists(Path.Combine(directory, name)))
                    throw new IOException("Gate directory contains stale control files");
            Interlocked.Exchange(ref _started, 0);
            Interlocked.Exchange(ref _finished, 0);
            Interlocked.Exchange(ref _dropped, 0);
            _release = NewRelease();
            Volatile.Write(ref _released, 0);
            Volatile.Write(ref _stop, 0);
            Volatile.Write(ref _armed, 1);
            var worker = new Thread(() => Watch(directory, expected))
            {
                IsBackground = true,
                Name = "benchmark async control",
            };
            try { worker.Start(); _worker = worker; }
            catch { Volatile.Write(ref _armed, 0); throw; }
        }
    }

    private static void WriteJson(string directory, string name, string value)
    {
        string temporary = Path.Combine(directory, name + ".tmp");
        File.WriteAllText(temporary, value);
        for (int attempt = 0; ; ++attempt)
        {
            try { File.Move(temporary, Path.Combine(directory, name), overwrite: true); return; }
            // Python may hold the previous snapshot open briefly on Windows.
            catch (IOException error) when (attempt < 19 && ((error.HResult & 0xffff) is 5 or 32))
            { Thread.Sleep(5); }
            catch (UnauthorizedAccessException) when (attempt < 19)
            { Thread.Sleep(5); }
        }
    }

    private static void Watch(string directory, int expected)
    {
        bool ready = false, acknowledged = false;
        string? previous = null;
        try
        {
            while (true)
            {
                long active = Active;
                if (!ready && Volatile.Read(ref _released) == 0 && active == expected)
                {
                    WriteJson(directory, "ready.json", FormattableString.Invariant($"{{\"active\":{active},\"expected\":{expected}}}"));
                    ready = true;
                }
                if (!acknowledged && File.Exists(Path.Combine(directory, "release")))
                {
                    WriteJson(directory, "released.json", FormattableString.Invariant($"{{\"active_before_release\":{active}}}"));
                    Release();
                    acknowledged = true;
                }
                string state = string.Create(CultureInfo.InvariantCulture,
                    $"{{\"active\":{Active},\"expected\":{expected},\"started\":{Interlocked.Read(ref _started)},\"finished\":{Finished},\"cancelled\":0,\"dropped\":{Interlocked.Read(ref _dropped)},\"released\":{(Volatile.Read(ref _released) != 0 ? "true" : "false")},\"control_error\":null}}");
                if (state != previous) { WriteJson(directory, "state.json", state); previous = state; }
                if (Volatile.Read(ref _stop) != 0) return;
                Thread.Sleep(1);
            }
        }
        catch (Exception error)
        {
            try { File.WriteAllText(Path.Combine(directory, "control-error.txt"), error.ToString()); }
            catch (Exception) { /* Missing acknowledgement also rejects the run. */ }
            Release();
        }
    }

    public static void Close()
    {
        lock (Gate)
        {
            Volatile.Write(ref _stop, 1);
            Release();
            _worker?.Join();
        }
    }
}
