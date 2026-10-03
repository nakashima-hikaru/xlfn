using System.Diagnostics;
using System.Threading;
using ExcelDna.Integration;
using ExcelDna.Registration;

namespace ExcelComparison;

public sealed class BenchAddIn : IExcelAddIn
{
    public void AutoOpen() { }
    public void AutoClose() => RtdEngine.Close();
}

public static class Functions
{
    private static long _sharedRead;
    private static long _contended;
    private static long _asyncActive;
    private static long _asyncFinished;
    private static readonly TaskCompletionSource<bool> Burst = new(TaskCreationOptions.RunContinuationsAsynchronously);

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
        Interlocked.Increment(ref _asyncActive);
        try
        {
            if (delayUs < 0)
                await Burst.Task.ConfigureAwait(false);
            else if (delayUs > 0 && delayUs < 1000)
                await Task.Run(() => Busy(delayUs)).ConfigureAwait(false);
            else if (delayUs > 0)
                await Task.Delay(TimeSpan.FromMilliseconds(delayUs / 1000.0)).ConfigureAwait(false);
            return value;
        }
        finally
        {
            Interlocked.Decrement(ref _asyncActive);
            Interlocked.Increment(ref _asyncFinished);
        }
    }

    [ExcelFunction(Name = "BENCH.ASYNC.RELEASE")]
    public static double AsyncRelease(double sequence)
    {
        Burst.TrySetResult(true);
        return sequence;
    }

    [ExcelFunction(Name = "BENCH.ASYNC.ACTIVE", IsThreadSafe = true)]
    public static double AsyncActive() => Interlocked.Read(ref _asyncActive);

    [ExcelFunction(Name = "BENCH.ASYNC.FINISHED", IsThreadSafe = true)]
    public static double AsyncFinished() => Interlocked.Read(ref _asyncFinished);

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
        public IExcelObserver Observer = observer;
        public double PeriodMs = periodMs;
        public long NextDue = Stopwatch.GetTimestamp() + (long)(periodMs * Stopwatch.Frequency / 1000);
        public long Sequence;
        public long LastPulse;
    }

    private sealed class Subscription(long id) : IDisposable
    {
        public void Dispose()
        {
            lock (Gate)
            {
                if (Entries.Remove(id, out Entry? entry) && entry.PeriodMs > 0)
                    --_periodicCount;
            }
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
        lock (Gate)
        {
            long pulse = Interlocked.Read(ref _pulse);
            if (pulse == _handledPulse && _periodicCount == 0) return;
            long now = Stopwatch.GetTimestamp();
            foreach (Entry entry in Entries.Values)
            {
                if (pulse > entry.LastPulse)
                {
                    entry.LastPulse = pulse;
                    try { entry.Observer.OnNext((double)pulse); } catch { /* disconnected */ }
                    Interlocked.Increment(ref _emitted);
                }
                else if (entry.PeriodMs > 0 && now >= entry.NextDue)
                {
                    try { entry.Observer.OnNext((double)++entry.Sequence); } catch { /* disconnected */ }
                    Interlocked.Increment(ref _emitted);
                    entry.NextDue = now + (long)(entry.PeriodMs * Stopwatch.Frequency / 1000);
                }
            }
            _handledPulse = pulse;
        }
    }

    public static void Close()
    {
        Thread? thread;
        lock (Gate)
        {
            _fastStop = true;
            thread = _fastThread;
            _fastThread = null;
            _timer?.Dispose();
            _timer = null;
            Entries.Clear();
            _periodicCount = 0;
        }
        thread?.Join(TimeSpan.FromSeconds(1));
    }
}
