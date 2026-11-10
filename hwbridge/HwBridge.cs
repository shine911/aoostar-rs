// HwBridge: a small standalone bridge that loads LibreHardwareMonitorLib
// (the upstream NuGet package, fetched by build.ps1; PawnIO-backed sensor
// access, same library AOOSTAR-X uses) and publishes CPU/GPU/motherboard/
// memory temperature and GPU load.
//
// Two output modes:
//   --shm             write sensor values into the "AOOSTAR_HW_STATS" shared
//                     memory region (slot 0 = HwBridge), consumed by
//                     asterctl --shm. No file I/O. Default mode for the
//                     launcher.
//   <file> [refresh]  legacy mode: write a plain "label: value" text file
//                     (default: cfg\sensors\hwbridge.txt) that asterctl
//                     picks up via its sensor file watcher.
//
// Usage: HwBridge.exe --shm [refresh] [--raw] [--verbose]
//        HwBridge.exe <file> [refresh] [--raw] [--verbose]
//   --raw      diagnostics: also publish every individual sensor under a
//              raw "hw_<HardwareType>_<SensorType>_<Name>" key (~140 keys
//              on a typical machine; nothing in asterctl consumes them, and
//              they make the payload ~10x larger, so they are off by default).
//   --verbose  print the "wrote N sensors" summary on every tick. By default
//              it is printed on the first successful tick and then only when
//              the published sensor count changes (or after an error), so the
//              launcher's hwbridge.log does not grow forever.
//
// The shared memory layout (little-endian, offsets relative to the slot) is
// defined in crates/aster-hwstats/src/lib.rs and MUST stay in sync:
//   0   magic        u32   = 0x48575331 ("HWS1")
//   4   version      u32   = 1
//   8   sequence     u64   = writer counter, always stored as an EVEN value
//                            on a complete snapshot; bumped to odd around
//                            the payload write (seqlock, see WriteSharedMemory)
//  16   timestamp_ms u64   = Unix epoch milliseconds
//  24   payload_len  u32
//  28   reserved     u32
//  32   payload            = "label: value\n" lines, UTF-8
//
// Requires Administrator privileges to run (same requirement as AOOSTAR-X
// itself, since PawnIO needs elevated access to talk to hardware registers).
//
// Written in a deliberately old-style C# (no tuples, no string
// interpolation, no LINQ) to compile with the plain .NET Framework
// compiler that ships with Windows, without needing the .NET SDK.
// Build with hwbridge\build.ps1 (fetches the pinned dependencies, then csc).

using System;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.IO.MemoryMappedFiles;
using System.Text;
using System.Threading;
using LibreHardwareMonitor.Hardware;

namespace HwBridge
{
    internal class UpdateVisitor : IVisitor
    {
        public void VisitComputer(IComputer computer)
        {
            computer.Traverse(this);
        }

        public void VisitHardware(IHardware hardware)
        {
            hardware.Update();
            IHardware[] subHardware = hardware.SubHardware;
            for (int i = 0; i < subHardware.Length; i++)
            {
                subHardware[i].Accept(this);
            }
        }

        public void VisitSensor(ISensor sensor)
        {
        }

        public void VisitParameter(IParameter parameter)
        {
        }
    }

    internal class SensorEntry
    {
        public IHardware Hardware;
        public ISensor Sensor;

        public SensorEntry(IHardware hardware, ISensor sensor)
        {
            Hardware = hardware;
            Sensor = sensor;
        }
    }

    internal class Program
    {
        // Allowed refresh intervals, in seconds, matching the launcher's
        // refresh_time option and aster-sysinfo's --refresh.
        private static readonly int[] AllowedRefresh = new int[] { 2, 5, 10, 30 };
        private const int DefaultRefreshSeconds = 5;

        // Shared memory protocol constants (see crates/aster-hwstats).
        private const string RegionName = "AOOSTAR_HW_STATS";
        private const long RegionSize = 128 * 1024;   // 2 slots x 64 KiB
        private const long SlotSize = 64 * 1024;
        private const long SlotOffset = 0;            // producer 0 = HwBridge
        private const long MagicOffset = 0;
        private const long VersionOffset = 4;
        private const long SequenceOffset = 8;
        private const long TimestampOffset = 16;
        private const long PayloadLenOffset = 24;
        private const long PayloadOffset = 32;
        private const int MaxPayload = (int)SlotSize - (int)PayloadOffset;
        private const uint Magic = 0x48575331;        // "HWS1"
        private const uint Version = 1;
        private static readonly DateTime UnixEpoch = new DateTime(1970, 1, 1, 0, 0, 0, DateTimeKind.Utc);

        // Monotonic writer counter, bumped by 2 on every snapshot write
        // (odd/even seqlock: the slot is marked odd while writing, even when
        // the snapshot is complete). Seeded from the slot when the region is
        // first opened so a restarted writer never re-emits old numbers.
        private static long Sequence = 0;

        // Shared memory handles, opened lazily and kept for the process
        // lifetime (keeping a handle also keeps the pagefile-backed section
        // alive for readers that open it later). Dropped on any error.
        private static MemoryMappedFile SharedMmf = null;
        private static MemoryMappedViewAccessor SharedView = null;

        private static bool Raw = false;
        private static bool Verbose = false;

        // Reused per tick to avoid garbage.
        private static readonly Dictionary<string, string> Values = new Dictionary<string, string>();
        private static readonly StringBuilder PayloadBuilder = new StringBuilder();
        private static readonly Dictionary<ISensor, string> RawKeys = new Dictionary<ISensor, string>();
        private static readonly List<SensorEntry> AllSensors = new List<SensorEntry>();
        private static int SensorSignature = -1;

        private static int Main(string[] args)
        {
            bool useShm = false;
            string outPath = "cfg\\sensors\\hwbridge.txt";
            int refreshSeconds = DefaultRefreshSeconds;
            List<string> positional = new List<string>();

            for (int i = 0; i < args.Length; i++)
            {
                string arg = args[i];
                if (arg == "--shm")
                {
                    useShm = true;
                }
                else if (arg == "--raw")
                {
                    Raw = true;
                }
                else if (arg == "--verbose")
                {
                    Verbose = true;
                }
                else if (arg.StartsWith("--", StringComparison.Ordinal))
                {
                    Console.Error.WriteLine("HwBridge: unknown flag '" + arg + "' ignored (known: --shm, --raw, --verbose).");
                }
                else
                {
                    positional.Add(arg);
                }
            }

            int next = 0;
            if (!useShm && positional.Count > next)
            {
                outPath = positional[next++];
            }
            if (positional.Count > next)
            {
                refreshSeconds = ParseRefresh(positional[next]);
            }

            if (useShm)
            {
                Console.WriteLine("HwBridge: writing sensors to shared memory region '" + RegionName + "' every " + refreshSeconds + "s. Ctrl+C to stop.");
            }
            else
            {
                string outDir = Path.GetDirectoryName(Path.GetFullPath(outPath));
                Directory.CreateDirectory(outDir);
                Console.WriteLine("HwBridge: writing to " + Path.GetFullPath(outPath) + " every " + refreshSeconds + "s. Ctrl+C to stop.");
            }
            Console.WriteLine("HwBridge: this needs Administrator privileges to read hardware sensors via PawnIO.");

            Computer computer = new Computer();
            computer.IsCpuEnabled = true;
            computer.IsGpuEnabled = true;
            computer.IsMemoryEnabled = true;
            computer.IsMotherboardEnabled = true;

            try
            {
                computer.Open();
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine("HwBridge: failed to open hardware monitor. Are you running this elevated (as Administrator)? " + ex.ToString());
                return 1;
            }

            UpdateVisitor visitor = new UpdateVisitor();
            int lastLoggedCount = -1;
            string lastError = null;

            while (true)
            {
                try
                {
                    computer.Accept(visitor);

                    // Sensors can appear after the first Update, so re-collect
                    // whenever the hardware/sensor totals change.
                    int signature = CountSensors(computer.Hardware);
                    if (signature != SensorSignature)
                    {
                        AllSensors.Clear();
                        RawKeys.Clear();
                        CollectSensors(computer.Hardware, AllSensors);
                        SensorSignature = signature;
                    }

                    Values.Clear();

                    // Optional diagnostics: every individual sensor under a
                    // descriptive raw key, to inspect the real sensor names
                    // on this hardware and refine the "best pick" logic.
                    if (Raw)
                    {
                        for (int i = 0; i < AllSensors.Count; i++)
                        {
                            SensorEntry entry = AllSensors[i];
                            if (!entry.Sensor.Value.HasValue)
                            {
                                continue;
                            }
                            string rawKey;
                            if (!RawKeys.TryGetValue(entry.Sensor, out rawKey))
                            {
                                rawKey = "hw_" + entry.Hardware.HardwareType + "_" + entry.Sensor.SensorType + "_" + Sanitize(entry.Sensor.Name);
                                RawKeys[entry.Sensor] = rawKey;
                            }
                            Values[rawKey] = entry.Sensor.Value.Value.ToString("F1", CultureInfo.InvariantCulture);
                        }
                    }

                    // Best-effort picks matching AOOSTAR-X's own panel label names directly,
                    // so no extra sensor-mapping.cfg entry is needed for these.
                    AddBestPick(Values, AllSensors, "cpu_temperature", HardwareType.Cpu, SensorType.Temperature,
                        new string[] { "Package", "Tctl", "Tdie", "Core (Tctl", "CPU Core" });

                    bool haveMobo = AddBestPick(Values, AllSensors, "motherboard_temperature", HardwareType.Motherboard, SensorType.Temperature,
                        new string[] { "System", "Motherboard", "CPU" });
                    if (!haveMobo)
                    {
                        AddBestPick(Values, AllSensors, "motherboard_temperature", HardwareType.SuperIO, SensorType.Temperature,
                            new string[] { "System", "Motherboard", "CPU" });
                    }

                    // Average of the per-DIMM readings ("DIMM #0", "DIMM #1", ...),
                    // matching AOOSTAR-X's "Average Temperature". A plain highest-value
                    // pick would grab the SPD hub's "Thermal Sensor Critical High Limit"
                    // (a fixed 85 C threshold), not a real temperature.
                    AddAverage(Values, AllSensors, "memory_Temperature", HardwareType.Memory, SensorType.Temperature, "DIMM");

                    AddBestPick(Values, AllSensors, "gpu_core", HardwareType.GpuNvidia, SensorType.Load, GpuLoadPrefs);
                    AddBestPick(Values, AllSensors, "gpu_temperature", HardwareType.GpuNvidia, SensorType.Temperature, GpuTempPrefs);
                    AddBestPick(Values, AllSensors, "gpu_core", HardwareType.GpuAmd, SensorType.Load, GpuLoadPrefs);
                    AddBestPick(Values, AllSensors, "gpu_temperature", HardwareType.GpuAmd, SensorType.Temperature, GpuTempPrefs);
                    AddBestPick(Values, AllSensors, "gpu_core", HardwareType.GpuIntel, SensorType.Load, GpuLoadPrefs);
                    AddBestPick(Values, AllSensors, "gpu_temperature", HardwareType.GpuIntel, SensorType.Temperature, GpuTempPrefs);

                    if (useShm)
                    {
                        WriteSharedMemory(Values);
                    }
                    else
                    {
                        WriteAtomic(outPath, Values);
                    }

                    // Log the first success, count changes and recovery from
                    // an error; every tick only with --verbose.
                    if (Verbose || lastLoggedCount != Values.Count)
                    {
                        string summary = "[" + DateTime.Now.ToString("HH:mm:ss") + "] wrote " + Values.Count + " sensors";
                        string v;
                        if (Values.TryGetValue("cpu_temperature", out v))
                        {
                            summary += ", cpu_temperature=" + v;
                        }
                        if (Values.TryGetValue("gpu_core", out v))
                        {
                            summary += ", gpu_core=" + v;
                        }
                        if (Values.TryGetValue("gpu_temperature", out v))
                        {
                            summary += ", gpu_temperature=" + v;
                        }
                        Console.WriteLine(summary);
                        lastLoggedCount = Values.Count;
                    }
                    lastError = null;
                }
                catch (Exception ex)
                {
                    // Drop the shared memory handles so the next tick reopens
                    // them; repeated identical errors are logged once.
                    CloseSharedMemory();
                    lastLoggedCount = -1;
                    if (lastError != ex.Message)
                    {
                        Console.Error.WriteLine("HwBridge: update error: " + ex.Message);
                        lastError = ex.Message;
                    }
                }

                Thread.Sleep(refreshSeconds * 1000);
            }
        }

        private static readonly string[] GpuLoadPrefs = new string[] { "GPU Core", "D3D" };
        private static readonly string[] GpuTempPrefs = new string[] { "GPU Core", "GPU Hot Spot" };

        // Parses a refresh interval argument, falling back to the default
        // (with a warning) when it is not one of the allowed values.
        private static int ParseRefresh(string value)
        {
            int parsed;
            if (int.TryParse(value, NumberStyles.None, CultureInfo.InvariantCulture, out parsed))
            {
                for (int i = 0; i < AllowedRefresh.Length; i++)
                {
                    if (AllowedRefresh[i] == parsed)
                    {
                        return parsed;
                    }
                }
            }
            Console.Error.WriteLine("HwBridge: invalid refresh interval '" + value + "', must be one of 2/5/10/30 seconds; using default " + DefaultRefreshSeconds + "s.");
            return DefaultRefreshSeconds;
        }

        // Cheap change detector: total hardware + sensor count over the tree.
        private static int CountSensors(IList<IHardware> hardwareList)
        {
            int total = 0;
            for (int i = 0; i < hardwareList.Count; i++)
            {
                IHardware hw = hardwareList[i];
                total += 1 + hw.Sensors.Length + CountSensors(hw.SubHardware);
            }
            return total;
        }

        private static void CollectSensors(IList<IHardware> hardwareList, List<SensorEntry> into)
        {
            for (int i = 0; i < hardwareList.Count; i++)
            {
                IHardware hw = hardwareList[i];
                ISensor[] sensors = hw.Sensors;
                for (int j = 0; j < sensors.Length; j++)
                {
                    into.Add(new SensorEntry(hw, sensors[j]));
                }
                CollectSensors(hw.SubHardware, into);
            }
        }

        private static bool Matches(SensorEntry entry, HardwareType hwType, SensorType sensorType)
        {
            return entry.Hardware.HardwareType == hwType && entry.Sensor.SensorType == sensorType && entry.Sensor.Value.HasValue;
        }

        private static bool AddBestPick(Dictionary<string, string> values, List<SensorEntry> all,
            string outKey, HardwareType hwType, SensorType sensorType, string[] namePreferences)
        {
            // First sensor (in collection order) matching the earliest
            // name preference wins.
            for (int p = 0; p < namePreferences.Length; p++)
            {
                string pref = namePreferences[p];
                for (int i = 0; i < all.Count; i++)
                {
                    SensorEntry entry = all[i];
                    if (Matches(entry, hwType, sensorType)
                        && entry.Sensor.Name.IndexOf(pref, StringComparison.OrdinalIgnoreCase) >= 0)
                    {
                        values[outKey] = entry.Sensor.Value.Value.ToString("F1", CultureInfo.InvariantCulture);
                        return true;
                    }
                }
            }

            // No preference matched: fall back to the highest value.
            ISensor best = null;
            for (int i = 0; i < all.Count; i++)
            {
                SensorEntry entry = all[i];
                if (Matches(entry, hwType, sensorType)
                    && (best == null || entry.Sensor.Value.Value > best.Value.Value))
                {
                    best = entry.Sensor;
                }
            }
            if (best == null)
            {
                return false;
            }
            values[outKey] = best.Value.Value.ToString("F1", CultureInfo.InvariantCulture);
            return true;
        }

        // Writes the mean of every sensor of the given hardware/sensor type whose
        // name contains nameContains. Returns false (writes nothing) if none has a value.
        private static bool AddAverage(Dictionary<string, string> values, List<SensorEntry> all,
            string outKey, HardwareType hwType, SensorType sensorType, string nameContains)
        {
            float sum = 0;
            int count = 0;
            for (int i = 0; i < all.Count; i++)
            {
                SensorEntry entry = all[i];
                if (entry.Hardware.HardwareType == hwType && entry.Sensor.SensorType == sensorType && entry.Sensor.Value.HasValue
                    && entry.Sensor.Name.IndexOf(nameContains, StringComparison.OrdinalIgnoreCase) >= 0)
                {
                    sum += entry.Sensor.Value.Value;
                    count++;
                }
            }
            if (count == 0)
            {
                return false;
            }
            values[outKey] = (sum / count).ToString("F1", CultureInfo.InvariantCulture);
            return true;
        }

        private static string Sanitize(string name)
        {
            string result = name.Replace(' ', '_');
            result = result.Replace('(', '_');
            result = result.Replace(')', '_');
            result = result.Replace('/', '_');
            return result;
        }

        private static void WriteAtomic(string outPath, Dictionary<string, string> values)
        {
            string dir = Path.GetDirectoryName(Path.GetFullPath(outPath));
            string tmpPath = Path.Combine(dir, "hwbridge.tmp." + Guid.NewGuid().ToString("N") + ".txt");

            StreamWriter writer = new StreamWriter(tmpPath, false);
            try
            {
                Dictionary<string, string>.Enumerator e = values.GetEnumerator();
                while (e.MoveNext())
                {
                    writer.WriteLine(e.Current.Key + ": " + e.Current.Value);
                }
            }
            finally
            {
                writer.Close();
            }

            if (File.Exists(outPath))
            {
                File.Delete(outPath);
            }
            File.Move(tmpPath, outPath);
        }

        // Opens (once) the shared region and slot view, seeding Sequence from
        // whatever the slot already holds. Keeping the handles open also
        // keeps the pagefile-backed section alive when no reader has it open.
        private static void EnsureSharedMemory()
        {
            if (SharedView != null)
            {
                return;
            }
            MemoryMappedFile mmf = null;
            MemoryMappedViewAccessor view = null;
            try
            {
                mmf = MemoryMappedFile.CreateOrOpen(RegionName, RegionSize, MemoryMappedFileAccess.ReadWrite);
                view = mmf.CreateViewAccessor(SlotOffset, SlotSize);

                // Never go backwards: readers treat an unchanged sequence as
                // "no new data". Round up to even (a crashed writer may have
                // left an odd in-progress value).
                if (view.ReadUInt32(MagicOffset) == Magic)
                {
                    long existing = (long)view.ReadUInt64(SequenceOffset);
                    existing += existing & 1;
                    if (existing > Sequence)
                    {
                        Sequence = existing;
                    }
                }
            }
            catch
            {
                if (view != null)
                {
                    view.Dispose();
                }
                if (mmf != null)
                {
                    mmf.Dispose();
                }
                throw;
            }
            SharedMmf = mmf;
            SharedView = view;
        }

        private static void CloseSharedMemory()
        {
            if (SharedView != null)
            {
                try { SharedView.Dispose(); } catch { }
                SharedView = null;
            }
            if (SharedMmf != null)
            {
                try { SharedMmf.Dispose(); } catch { }
                SharedMmf = null;
            }
        }

        // Serializes the sensor values into the "label: value\n" payload and
        // publishes it into slot 0 of the AOOSTAR_HW_STATS shared memory
        // region. Odd/even seqlock: the sequence is first written as the odd
        // "in progress" marker, then the payload, then the final even value,
        // so readers never observe a half-written snapshot.
        private static void WriteSharedMemory(Dictionary<string, string> values)
        {
            StringBuilder sb = PayloadBuilder;
            sb.Length = 0;
            Dictionary<string, string>.Enumerator e = values.GetEnumerator();
            while (e.MoveNext())
            {
                sb.Append(e.Current.Key);
                sb.Append(": ");
                sb.Append(e.Current.Value);
                sb.Append('\n');
            }

            byte[] payload = Encoding.UTF8.GetBytes(sb.ToString());
            if (payload.Length > MaxPayload)
            {
                byte[] truncated = new byte[MaxPayload];
                Buffer.BlockCopy(payload, 0, truncated, 0, MaxPayload);
                payload = truncated;
            }

            EnsureSharedMemory();
            MemoryMappedViewAccessor view = SharedView;

            long nowMs = (long)(DateTime.UtcNow - UnixEpoch).TotalMilliseconds;
            long newSeq = Sequence + 2; // even: snapshot-complete value

            // 1. Mark the snapshot as in progress (odd).
            view.Write(SequenceOffset, newSeq - 1);
            Thread.MemoryBarrier();

            // 2. Header + payload.
            view.Write(MagicOffset, Magic);
            view.Write(VersionOffset, Version);
            view.Write(TimestampOffset, nowMs);
            view.WriteArray<byte>(PayloadOffset, payload, 0, payload.Length);
            view.Write(PayloadLenOffset, payload.Length);

            // 3. Publish: store the final even value. The barrier makes all
            // payload writes visible before the bump readers synchronize on.
            Thread.MemoryBarrier();
            view.Write(SequenceOffset, newSeq);
            Sequence = newSeq;
        }
    }
}
