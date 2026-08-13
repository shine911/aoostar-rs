// HwBridge: a small standalone bridge that loads AOOSTAR-X's own
// LibreHardwareMonitorLib.dll (same DLL, same PawnIO-backed sensor access)
// and writes CPU/GPU/motherboard/memory temperature and GPU load into a
// plain "label: value" text file inside aster-sysinfo's cfg\sensors\
// directory, so asterctl picks it up exactly like any other sensor source.
//
// Requires Administrator privileges to run (same requirement as AOOSTAR-X
// itself, since PawnIO needs elevated access to talk to hardware registers).
//
// Written in a deliberately old-style C# (no tuples, no string
// interpolation, no LINQ) to compile with the plain .NET Framework
// compiler that ships with Windows, without needing the .NET SDK.

using System;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
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

        private static int Main(string[] args)
        {
            string outPath = "cfg\\sensors\\hwbridge.txt";
            int refreshSeconds = DefaultRefreshSeconds;

            if (args.Length > 0)
            {
                outPath = args[0];
            }
            if (args.Length > 1)
            {
                refreshSeconds = ParseRefresh(args[1]);
            }

            string outDir = Path.GetDirectoryName(Path.GetFullPath(outPath));
            Directory.CreateDirectory(outDir);

            Console.WriteLine("HwBridge: writing to " + Path.GetFullPath(outPath) + " every " + refreshSeconds + "s. Ctrl+C to stop.");
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

            while (true)
            {
                try
                {
                    computer.Accept(visitor);

                    List<SensorEntry> allSensors = new List<SensorEntry>();
                    CollectSensors(computer.Hardware, allSensors);

                    Dictionary<string, string> values = new Dictionary<string, string>();

                    // Dump every individual sensor too, under a descriptive raw key --
                    // useful for inspecting the real sensor names on this hardware and
                    // refining the "best pick" logic below if it guesses wrong.
                    for (int i = 0; i < allSensors.Count; i++)
                    {
                        SensorEntry entry = allSensors[i];
                        if (!entry.Sensor.Value.HasValue)
                        {
                            continue;
                        }
                        string rawKey = "hw_" + entry.Hardware.HardwareType + "_" + entry.Sensor.SensorType + "_" + Sanitize(entry.Sensor.Name);
                        values[rawKey] = entry.Sensor.Value.Value.ToString("F1", CultureInfo.InvariantCulture);
                    }

                    // Best-effort picks matching AOOSTAR-X's own panel label names directly,
                    // so no extra sensor-mapping.cfg entry is needed for these.
                    AddBestPick(values, allSensors, "cpu_temperature", HardwareType.Cpu, SensorType.Temperature,
                        new string[] { "Package", "Tctl", "Tdie", "Core (Tctl", "CPU Core" });

                    bool haveMobo = AddBestPick(values, allSensors, "motherboard_temperature", HardwareType.Motherboard, SensorType.Temperature,
                        new string[] { "System", "Motherboard", "CPU" });
                    if (!haveMobo)
                    {
                        AddBestPick(values, allSensors, "motherboard_temperature", HardwareType.SuperIO, SensorType.Temperature,
                            new string[] { "System", "Motherboard", "CPU" });
                    }

                    AddBestPick(values, allSensors, "memory_Temperature", HardwareType.Memory, SensorType.Temperature, new string[0]);

                    AddBestPick(values, allSensors, "gpu_core", HardwareType.GpuNvidia, SensorType.Load, new string[] { "GPU Core", "D3D" });
                    AddBestPick(values, allSensors, "gpu_temperature", HardwareType.GpuNvidia, SensorType.Temperature, new string[] { "GPU Core", "GPU Hot Spot" });
                    AddBestPick(values, allSensors, "gpu_core", HardwareType.GpuAmd, SensorType.Load, new string[] { "GPU Core", "D3D" });
                    AddBestPick(values, allSensors, "gpu_temperature", HardwareType.GpuAmd, SensorType.Temperature, new string[] { "GPU Core", "GPU Hot Spot" });
                    AddBestPick(values, allSensors, "gpu_core", HardwareType.GpuIntel, SensorType.Load, new string[] { "GPU Core", "D3D" });
                    AddBestPick(values, allSensors, "gpu_temperature", HardwareType.GpuIntel, SensorType.Temperature, new string[] { "GPU Core", "GPU Hot Spot" });

                    WriteAtomic(outPath, values);

                    string summary = "[" + DateTime.Now.ToString("HH:mm:ss") + "] wrote " + values.Count + " sensors";
                    if (values.ContainsKey("cpu_temperature"))
                    {
                        summary += ", cpu_temperature=" + values["cpu_temperature"];
                    }
                    if (values.ContainsKey("gpu_core"))
                    {
                        summary += ", gpu_core=" + values["gpu_core"];
                    }
                    if (values.ContainsKey("gpu_temperature"))
                    {
                        summary += ", gpu_temperature=" + values["gpu_temperature"];
                    }
                    Console.WriteLine(summary);
                }
                catch (Exception ex)
                {
                    Console.Error.WriteLine("HwBridge: update error: " + ex.Message);
                }

                Thread.Sleep(refreshSeconds * 1000);
            }
        }

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

        private static bool AddBestPick(Dictionary<string, string> values, List<SensorEntry> all,
            string outKey, HardwareType hwType, SensorType sensorType, string[] namePreferences)
        {
            List<ISensor> candidates = new List<ISensor>();
            for (int i = 0; i < all.Count; i++)
            {
                SensorEntry entry = all[i];
                if (entry.Hardware.HardwareType == hwType && entry.Sensor.SensorType == sensorType && entry.Sensor.Value.HasValue)
                {
                    candidates.Add(entry.Sensor);
                }
            }
            if (candidates.Count == 0)
            {
                return false;
            }

            for (int p = 0; p < namePreferences.Length; p++)
            {
                string pref = namePreferences[p];
                for (int c = 0; c < candidates.Count; c++)
                {
                    if (candidates[c].Name.IndexOf(pref, StringComparison.OrdinalIgnoreCase) >= 0)
                    {
                        values[outKey] = candidates[c].Value.Value.ToString("F1", CultureInfo.InvariantCulture);
                        return true;
                    }
                }
            }

            ISensor best = candidates[0];
            for (int c = 1; c < candidates.Count; c++)
            {
                if (candidates[c].Value.Value > best.Value.Value)
                {
                    best = candidates[c];
                }
            }
            values[outKey] = best.Value.Value.ToString("F1", CultureInfo.InvariantCulture);
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
    }
}
