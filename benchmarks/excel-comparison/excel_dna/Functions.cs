using System.Diagnostics;
using System.Threading;
using ExcelDna.Integration;
using ExcelDna.Registration;

namespace ExcelComparison;

public sealed class BenchAddIn : IExcelAddIn
{
    public void AutoOpen() { }
    public void AutoClose()
    {
        try { RtdEngine.Close(); }
        finally { AsyncGateControl.Close(); }
    }
}

public static class Functions
{
    private static long _sharedRead;
    private static long _contended;

    [ExcelFunction(Name = "BENCH.ID", IsThreadSafe = true)]
    public static double Identity(double value) => value;

    [ExcelFunction(Name = "BENCH.SUM2", IsThreadSafe = true)]
    public static double Sum2(double a, double b) => a + b;

    [ExcelFunction(Name = "BENCH.SUM4", IsThreadSafe = true)]
    public static double Sum4(double a, double b, double c, double d) => a + b + c + d;

    [ExcelFunction(Name = "BENCH.SUM8", IsThreadSafe = true)]
    public static double Sum8(double a, double b, double c, double d, double e, double f, double g, double h)
        => a + b + c + d + e + f + g + h;

    private static void Busy(int micros)
    {
        if (micros <= 0) return;
        long start = Stopwatch.GetTimestamp();
        long target = (long)(micros * (double)Stopwatch.Frequency / 1_000_000);
        while (Stopwatch.GetTimestamp() - start < target) Thread.SpinWait(1);
    }

    [ExcelFunction(Name = "BENCH.CPU", IsThreadSafe = true)]
    public static double Cpu(double value, int micros) { Busy(micros); return value; }

    [ExcelFunction(Name = "BENCH.ERRNUM", IsThreadSafe = true)]
    public static object Error(double value, int period)
        => period > 0 && (long)value % period == 0 ? ExcelError.ExcelErrorNum : value;

    [ExcelFunction(Name = "BENCH.MAT.SUM", IsThreadSafe = true)]
    public static double MatrixSum(double[,] values)
    {
        double sum = 0;
        foreach (double value in values) sum += value;
        return sum;
    }

    [ExcelFunction(Name = "BENCH.MAT.MAKE", IsThreadSafe = true)]
    public static double[,] MatrixMake(int rows, int columns, double seed)
    {
        var result = new double[rows, columns];
        for (int row = 0; row < rows; row++)
            for (int column = 0; column < columns; column++)
                result[row, column] = seed + row * columns + column;
        return result;
    }

    [ExcelFunction(Name = "BENCH.MAT.COPY", IsThreadSafe = true)]
    public static double[,] MatrixCopy(double[,] values) => values;

    [ExcelFunction(Name = "BENCH.MAT.MIXED", IsThreadSafe = true)]
    public static double MatrixMixed(object[,] values)
    {
        double score = 0;
        foreach (object value in values)
            score += value switch
            {
                double or int => 1,
                string => 2,
                bool => 3,
                ExcelError => 5,
                _ => 4,
            };
        return score;
    }

    [ExcelFunction(Name = "BENCH.STR.IN", IsThreadSafe = true)]
    public static double StringInput(string value) => value.Length;

    [ExcelFunction(Name = "BENCH.STR.OUT", IsThreadSafe = true)]
    public static string StringOutput(int length, bool japanese)
        => new string(japanese ? '日' : 'a', Math.Max(0, length));

    [ExcelFunction(Name = "BENCH.STR.COPY", IsThreadSafe = true)]
    public static string StringCopy(string value) => value;

    [ExcelFunction(Name = "BENCH.SHARED", IsThreadSafe = true)]
    public static double SharedRead(double value) => value + Interlocked.Read(ref _sharedRead);

    [ExcelFunction(Name = "BENCH.CONTENDED", IsThreadSafe = true)]
    public static double Contended(double value) => value + Interlocked.Increment(ref _contended) - 1;

    // Managed allocation bytes exclude Excel-DNA's native loader and Excel.
    [ExcelFunction(Name = "BENCH.ALLOC.BYTES", IsThreadSafe = true)]
    public static double AllocBytes() => GC.GetTotalAllocatedBytes(true);

    // Matching control for the harness. The CLR's cumulative counter is always
    // available, so enabling or disabling the observation window is a no-op.
    [ExcelFunction(Name = "BENCH.ALLOC.TRACK")]
    public static double AllocTrack(bool enabled) => enabled ? 1.0 : 0.0;

    [ExcelAsyncFunction(Name = "BENCH.ASYNC")]
    public static async Task<double> AsyncValue(double value, int delayUs)
    {
        AsyncGateControl.Enter();
        bool completed = false;
        try
        {
            if (delayUs < 0)
                await AsyncGateControl.WaitAsync().ConfigureAwait(false);
            else if (delayUs > 0 && delayUs < 1000)
                await Task.Run(() => Busy(delayUs)).ConfigureAwait(false);
            else if (delayUs > 0)
                await Task.Delay(TimeSpan.FromMilliseconds(delayUs / 1000.0)).ConfigureAwait(false);
            completed = true;
            return value;
        }
        finally { AsyncGateControl.Finish(completed); }
    }

    [ExcelFunction(Name = "BENCH.ASYNC.ARM")]
    public static double AsyncArm(string directory, int expected)
    {
        AsyncGateControl.Arm(directory, expected);
        return 1;
    }

    [ExcelFunction(Name = "BENCH.ASYNC.RELEASE")]
    public static double AsyncRelease(double sequence)
    {
        AsyncGateControl.Release();
        return sequence;
    }

    [ExcelFunction(Name = "BENCH.ASYNC.ACTIVE", IsThreadSafe = true)]
    public static double AsyncActive() => AsyncGateControl.Active;

    [ExcelFunction(Name = "BENCH.ASYNC.FINISHED", IsThreadSafe = true)]
    public static double AsyncFinished() => AsyncGateControl.Finished;

    [ExcelFunction(Name = "BENCH.RTD")]
    public static object Rtd(string topic, double periodMs)
        => ExcelAsyncUtil.Observe("BENCH.RTD", new object[] { topic, periodMs },
            () => new BenchObservable(topic, periodMs));

    [ExcelFunction(Name = "BENCH.RTD.EMITTED", IsThreadSafe = true)]
    public static double RtdEmitted() => RtdEngine.Emitted;

    [ExcelFunction(Name = "BENCH.RTD.PULSE")]
    public static double RtdPulse(double sequence)
    {
        RtdEngine.Pulse((long)sequence);
        return sequence;
    }

    [ExcelFunction(Name = "BENCH.RTD.COUNT", IsThreadSafe = true)]
    public static double RtdCount() => RtdEngine.Count;
}

internal sealed class BenchObservable(string topic, double periodMs) : IExcelObservable
{
    public IDisposable Subscribe(IExcelObserver observer)
        => RtdEngine.Subscribe(topic, periodMs, observer);
}

internal static class RtdEngine
{
    private sealed class Entry(IExcelObserver observer, double periodMs)
    {
        private readonly object _publicationGate = new();
        private IExcelObserver? _observer = observer;
        private int _cancelled;
        public readonly double PeriodMs = periodMs;
        private long _nextDue = Stopwatch.GetTimestamp() + (long)(periodMs * Stopwatch.Frequency / 1000);
        private long _sequence;
        public long LastPulse;

        public void Cancel() => Volatile.Write(ref _cancelled, 1);

        public void Drain()
        {
            // Subscription removal releases Gate before waiting for an
            // admitted callback. No callback can start after cancellation.
            lock (_publicationGate) _observer = null;
        }

        public void Publish(long pulse, long now)
        {
            lock (_publicationGate)
            {
                if (Volatile.Read(ref _cancelled) != 0 || _observer is null) return;
                long value;
                if (pulse > LastPulse)
                {
                    LastPulse = pulse;
                    value = pulse;
                }
                else if (PeriodMs > 0 && now >= _nextDue)
                {
                    value = ++_sequence;
                    _nextDue = now + (long)(PeriodMs * Stopwatch.Frequency / 1000);
                }
                else return;
                try { _observer.OnNext((double)value); } catch { /* disconnected */ }
                Interlocked.Increment(ref _emitted);
            }
        }
    }

    private sealed class Subscription(long id) : IDisposable
    {
        public void Dispose()
        {
            Entry? removed;
            lock (Gate)
            {
                Entries.Remove(id, out removed);
                if (removed is not null)
                {
                    removed.Cancel();
                    if (removed.PeriodMs > 0) --_periodicCount;
                }
            }
            removed?.Drain();
        }
    }

    private static readonly object Gate = new();
    private static readonly Dictionary<long, Entry> Entries = new();
    private static Timer? _timer;
    private static Thread? _fastThread;
    private static volatile bool _fastStop;
    private static long _nextId;
    private static long _pulse;
    private static long _handledPulse;
    private static int _periodicCount;
    private static long _emitted;
    [ThreadStatic] private static List<Entry>? _snapshot;

    public static int Count { get { lock (Gate) return Entries.Count; } }
    public static long Emitted => Interlocked.Read(ref _emitted);

    public static IDisposable Subscribe(string topic, double periodMs, IExcelObserver observer)
    {
        _ = topic;
        lock (Gate)
        {
            _timer ??= new Timer(Tick, null, 1, 1);
            long id = ++_nextId;
            double period = Math.Max(0, periodMs);
            if (period > 0) ++_periodicCount;
            if (period > 0 && period < 1 && _fastThread is null)
            {
                _fastStop = false;
                _fastThread = new Thread(() =>
                {
                    while (!_fastStop)
                    {
                        Tick(null);
                        Thread.SpinWait(128);
                    }
                }) { IsBackground = true, Name = "Excel comparison fast RTD source" };
                _fastThread.Start();
            }
            Entries.Add(id, new Entry(observer, period) { LastPulse = _pulse });
            return new Subscription(id);
        }
    }

    public static void Pulse(long sequence) => Interlocked.Exchange(ref _pulse, sequence);

    private static void Tick(object? state)
    {
        _ = state;
        List<Entry> snapshot;
        long pulse;
        long now;
        lock (Gate)
        {
            pulse = Interlocked.Read(ref _pulse);
            if (pulse == _handledPulse && _periodicCount == 0) return;
            now = Stopwatch.GetTimestamp();
            snapshot = _snapshot ??= new List<Entry>();
            snapshot.AddRange(Entries.Values);
            _handledPulse = pulse;
        }
        // Match the Rust fixture: source bookkeeping never holds its map
        // lock across the observer's framework notification.
        try { foreach (Entry entry in snapshot) entry.Publish(pulse, now); }
        finally { snapshot.Clear(); }
    }

    public static void Close()
    {
        Thread? thread;
        Entry[] removed;
        lock (Gate)
        {
            _fastStop = true;
            thread = _fastThread;
            _fastThread = null;
            _timer?.Dispose();
            _timer = null;
            removed = new Entry[Entries.Count];
            Entries.Values.CopyTo(removed, 0);
            foreach (Entry entry in removed) entry.Cancel();
            Entries.Clear();
            _periodicCount = 0;
        }
        foreach (Entry entry in removed) entry.Drain();
        thread?.Join(TimeSpan.FromSeconds(1));
    }
}
