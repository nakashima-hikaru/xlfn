using System.Diagnostics;
using System.Text.Json;
using ExcelComparison;

static void Check(bool condition, string message)
{
    if (!condition) throw new Exception(message);
}

static JsonElement WaitJson(string directory, string name, Func<JsonElement, bool>? predicate = null)
{
    var clock = Stopwatch.StartNew();
    while (clock.Elapsed < TimeSpan.FromSeconds(10))
    {
        if (File.Exists(Path.Combine(directory, "control-error.txt")))
            throw new Exception(File.ReadAllText(Path.Combine(directory, "control-error.txt")));
        string path = Path.Combine(directory, name);
        if (File.Exists(path))
        {
            using var json = JsonDocument.Parse(File.ReadAllText(path));
            if (predicate is null || predicate(json.RootElement)) return json.RootElement.Clone();
        }
        Thread.Sleep(1);
    }
    throw new TimeoutException($"Missing expected {name}");
}

string directory = Path.Combine(Path.GetTempPath(), "xlfn-dna-control-" + Guid.NewGuid());
Directory.CreateDirectory(directory);
try
{
    foreach (int invalid in new[] { 0, 4097 })
    {
        bool rejected = false;
        try { AsyncGateControl.Arm(directory, invalid); }
        catch (ArgumentException) { rejected = true; }
        Check(rejected, "Invalid capacity accepted");
    }
    File.WriteAllText(Path.Combine(directory, "release"), "");
    bool staleRejected = false;
    try { AsyncGateControl.Arm(directory, 2); }
    catch (IOException) { staleRejected = true; }
    Check(staleRejected, "Stale release accepted");
    File.Delete(Path.Combine(directory, "release"));

    AsyncGateControl.Arm(directory, 2);
    bool rearmRejected = false;
    try { AsyncGateControl.Arm(directory, 2); }
    catch (InvalidOperationException) { rearmRejected = true; }
    Check(rearmRejected, "An armed gate was reset");

    AsyncGateControl.Enter();
    Task first = AsyncGateControl.WaitAsync();
    bool emergency = args.Contains("emergency");
    if (!emergency)
    {
        AsyncGateControl.Enter();
        var ready = WaitJson(directory, "ready.json");
        Check(ready.GetProperty("active").GetInt64() == 2, "Wrong ready count");
        Check(!first.IsCompleted, "Tasks completed before independent release");
    }
    File.WriteAllText(Path.Combine(directory, "release"), "");
    var released = WaitJson(directory, "released.json");
    Check(released.GetProperty("active_before_release").GetInt64() == (emergency ? 1 : 2), "Release evidence lost pending count");
    Check(first.Wait(TimeSpan.FromSeconds(10)), "Independent release failed");
    AsyncGateControl.Finish(completed: true);
    if (!emergency) AsyncGateControl.Finish(completed: false);
    var state = WaitJson(directory, "state.json", json => json.GetProperty("released").GetBoolean()
        && json.GetProperty("active").GetInt64() == 0);
    Check(state.GetProperty("finished").GetInt64() == (emergency ? 1 : 2), "Finished count missing");
    Check(state.GetProperty("dropped").GetInt64() == (emergency ? 0 : 1), "Dropped task was marked complete");
    if (emergency) Check(!File.Exists(Path.Combine(directory, "ready.json")), "Emergency release claimed readiness");
    Console.WriteLine($"Async control {(emergency ? "emergency" : "normal")} checks passed");
}
finally
{
    AsyncGateControl.Close();
    Directory.Delete(directory, recursive: true);
}
